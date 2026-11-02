/*! Painel flux — cliente do /events novo (fatia 4, 02/11/2026).
   Regras da casa, inegociáveis:
   - NUNCA injetar HTML com dado do servidor: mensagens entram só por
     x-text (textContent) — o XSS da era do swap de markup não volta.
     (Este arquivo não contém a API de injeção NEM a palavra: um teste
     do Rust garante.)
   - Hora LOCAL do navegador; UTC fica no `cru` (escopo decidido com o
     Gabriel).
   - Append no topo COM scroll compensado: nada desliza sob os olhos.
   - Agrupa repetidos em sequência (escopo+mensagem+nível) com contador ×N.
   Componentes registrados via Alpine.data (porta aberta pro build CSP). */
document.addEventListener('alpine:init', () => {
  Alpine.data('eventosPage', () => ({
    escopos: [],
    escopo: '',
    nivel: '',
    busca: '',
    eventos: [], // grupos, mais novo primeiro
    total: 0,
    pagina: 1,
    paginas: 1,
    carregando: false,
    fim: false,
    pausado: false,
    retidos: 0,
    vivo: 'conectando…',
    notif: (window.Notification && Notification.permission) || 'indisponivel',
    medidor: { req: 0, err429: 0, err5xx: 0 },
    ultimoId: 0,
    es: null,
    timerBusca: null,
    observer: null,
    janela: [], // access dos últimos 5 min {t, st}

    // MESMO formato de events.rs::linha_access — os dois lados têm de
    // concordar; o teste Rust fixa a forma.
    RE_ACCESS: /^(\S+) (\S+) (\d{3}) (\d+)ms host=([^\s]+)/,

    async init() {
      const salvo = JSON.parse(localStorage.getItem('flux.eventos.filtros') || '{}');
      this.escopo = salvo.escopo || '';
      this.nivel = salvo.nivel || '';
      this.busca = salvo.busca || '';
      await this.recarregar();
      this.conectar();
      this.observer = new IntersectionObserver((entradas) => {
        if (entradas[0].isIntersecting) this.proximaPagina();
      });
      this.observer.observe(this.$refs.sentinel);
      setInterval(() => this.podaJanela(), 30000);
    },

    urlFiltros(extra) {
      const qs = new URLSearchParams();
      if (this.escopo) qs.set('escopo', this.escopo);
      if (this.nivel) qs.set('nivel', this.nivel);
      if (this.busca) qs.set('q', this.busca);
      if (extra) for (const [k, v] of Object.entries(extra)) qs.set(k, v);
      return qs.toString();
    },

    async recarregar() {
      this.carregando = true;
      try {
        const r = await fetch('/api/events?' + this.urlFiltros({ page: 1 }));
        if (!r.ok) throw new Error('api ' + r.status);
        const d = await r.json();
        this.escopos = d.escopos || [];
        this.total = d.total;
        this.pagina = d.pagina;
        this.paginas = d.paginas;
        this.eventos = [];
        for (const ev of [...d.eventos].reverse()) this.dobra(ev, false);
        this.ultimoId = d.eventos.length ? Math.max(...d.eventos.map((e) => e.id)) : this.ultimoId;
        this.fim = d.pagina >= d.paginas;
      } catch (e) {
        this.vivo = 'falhou: ' + e.message;
      } finally {
        this.carregando = false;
      }
    },

    // Filtro mudou: persiste, recarrega a lista e reconecta o SSE com a
    // query nova (o servidor filtra de novo — fonte única no SQL).
    filtra() {
      localStorage.setItem(
        'flux.eventos.filtros',
        JSON.stringify({ escopo: this.escopo, nivel: this.nivel, busca: this.busca }),
      );
      this.eventos = [];
      this.ultimoId = 0;
      this.pagina = 1;
      this.fim = false;
      this.recarregar().then(() => this.conectar());
    },

    buscaAoVivo() {
      clearTimeout(this.timerBusca);
      this.timerBusca = setTimeout(() => this.filtra(), 300);
    },

    conectar() {
      if (this.es) this.es.close();
      this.es = new EventSource('/api/events/follow?' + this.urlFiltros({ desde: this.ultimoId }));
      this.es.onopen = () => {
        this.vivo = 'ao vivo';
      };
      this.es.onerror = () => {
        this.vivo = 'reconectando…';
      };
      this.es.onmessage = (m) => {
        try {
          this.chegou(JSON.parse(m.data));
        } catch (_) {
          /* frame não-JSON (ping é comentário, nem chega aqui) */
        }
      };
    },

    // Evento do stream → grupo. Duplicata (id velho) morre aqui.
    chegou(ev) {
      if (ev.id <= this.ultimoId) return;
      this.ultimoId = ev.id;
      this.medir(ev);
      this.avisar(ev);
      if (this.pausado) {
        this.retidos++;
        return;
      }
      this.dobra(ev, true);
    },

    // Junta no grupo de cima quando escopo+mensagem+nível coincidem (×N).
    dobra(ev, aoVivo) {
      const g = this.grupoDe(ev);
      const topo = this.eventos[0];
      if (topo && topo.chave === g.chave) {
        topo.count++;
        topo.times.unshift(g.times[0]);
        topo.ultimo = g.ultimo;
        this.eventos.splice(0, 1, { ...topo });
      } else {
        this.eventos.unshift(g);
        if (aoVivo) this.compensaRolagem();
        if (this.eventos.length > 500) this.eventos.pop(); // teto de DOM
      }
    },

    grupoDe(ev) {
      const m = this.RE_ACCESS.exec(ev.message);
      const grave = ev.level === 'warn' || ev.level === 'error' || ev.level === 'err';
      const g = {
        chave: ev.scope + '\u0000' + ev.message,
        message: ev.message,
        scope: ev.scope,
        level: ev.level,
        times: [ev.created_at],
        ultimo: ev.created_at,
        count: 1,
        dot: ev.level === 'warn' ? 'warn' : grave && ev.level !== 'warn' ? 'err' : 'idle',
        linha: ev.level === 'warn' ? 'lwarn' : grave && ev.level !== 'warn' ? 'lerr' : '',
        access: null,
      };
      if (m) {
        g.access = {
          metodo: m[1],
          caminho: m[2],
          status: m[3],
          st: this.stClasse(+m[3]),
          dur: m[4],
          host: m[5],
        };
      }
      return g;
    },

    // Mesmo mapeamento de formato.rs::classe_status.
    stClasse(s) {
      return s >= 500 || s === 429
        ? 'st-err'
        : s >= 400
          ? 'st-warn'
          : s >= 300
            ? 'st-3'
            : 'st-ok';
    },

    // Append no topo não desliza o conteúdo: mede o empurrão do corpo da
    // tabela e devolve em scroll (só se você já estava rolado).
    compensaRolagem() {
      if (window.scrollY <= 0) return;
      this.$nextTick(() => {
        const topo = this.$refs.corpo.getBoundingClientRect().top;
        if (topo > 0) window.scrollBy(0, topo);
      });
    },

    async proximaPagina() {
      if (this.carregando || this.fim || this.pagina >= this.paginas) return;
      this.carregando = true;
      this.pagina++;
      try {
        const r = await fetch('/api/events?' + this.urlFiltros({ page: this.pagina }));
        if (!r.ok) throw new Error('api ' + r.status);
        const d = await r.json();
        for (const ev of d.eventos) {
          // DESC: apenda no fim; dobra com a calda se a sequência continua.
          const g = this.grupoDe(ev);
          const calda = this.eventos[this.eventos.length - 1];
          if (calda && calda.chave === g.chave) {
            calda.count++;
            calda.times.push(g.times[0]);
            this.eventos.splice(this.eventos.length - 1, 1, { ...calda });
          } else {
            this.eventos.push(g);
          }
        }
        this.fim = d.pagina >= d.paginas;
      } catch (_) {
        this.pagina--;
      }
      this.carregando = false;
    },

    medir(ev) {
      const m = this.RE_ACCESS.exec(ev.message);
      if (!m) return;
      this.janela.push({ t: Date.now() / 1000, st: +m[3] });
      this.podaJanela();
    },

    podaJanela() {
      const corte = Date.now() / 1000 - 300;
      this.janela = this.janela.filter((x) => x.t >= corte);
      this.medidor = {
        req: Math.round(this.janela.length / 5),
        err429: this.janela.filter((x) => x.st === 429).length,
        err5xx: this.janela.filter((x) => x.st >= 500).length,
      };
    },

    // warn/error/acessos-anomalos acordam via Notification — só com a aba
    // em background (vendo a lista, notificação é ruído).
    avisar(ev) {
      const grave = ev.level === 'warn' || ev.level === 'error' || ev.level === 'err';
      if (!grave || this.notif !== 'granted' || !document.hidden) return;
      try {
        new Notification('flux: ' + ev.scope, {
          body: ev.message.slice(0, 140),
          tag: 'flux-' + ev.id,
        });
      } catch (_) {
        /* Notification sem service worker pode falhar — silêncio aceitável aqui */
      }
    },

    async pedirAvisos() {
      if (!window.Notification) return;
      this.notif = await Notification.requestPermission();
    },

    alternarPausa() {
      if (this.pausado) {
        this.pausado = false;
        this.retidos = 0;
        // Retidos voltam pela API (página 1 contém tudo que entrou — o
        // merge de dobra() mata repetido pelo id).
        this.recarregar();
      } else {
        this.pausado = true;
      }
    },

    // "YYYY-MM-DD HH:MM:SS" (UTC do banco) → hora LOCAL do navegador.
    quando(utc) {
      const d = new Date(utc.replace(' ', 'T') + 'Z');
      if (isNaN(d)) return utc;
      const p = (n) => String(n).padStart(2, '0');
      return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
    },
  }));
});

// ------------------------------------------------------------------ dashboard
document.addEventListener('alpine:init', () => {
  Alpine.data('dashboardPage', () => ({
    snap: null,
    saude: null,
    saudeErro: '',
    saudeAberto: false,
    aviso: '',
    vivo: 'conectando…',
    acordando: {},
    es: null,

    init() {
      const q = new URLSearchParams(location.search);
      if (q.get('rescan') === '1') this.aviso = 'rescan na fila — o flux watch executa';
      else if (q.get('rescan') === 'falha') {
        this.aviso = 'rescan recusado — o motivo está nos eventos (escopo painel)';
      }
      this.puxaSaude();
      setInterval(() => this.puxaSaude(), 60000);
      this.conectar();
    },

    async puxaSaude() {
      try {
        const r = await fetch('/api/saude');
        if (!r.ok) throw new Error('api ' + r.status);
        this.saude = await r.json();
        this.saudeErro = '';
      } catch (e) {
        this.saudeErro = String(e.message || e);
      }
    },

    conectar() {
      this.es = new EventSource('/api/dashboard/follow');
      this.es.onopen = () => {
        this.vivo = 'ao vivo';
      };
      this.es.onerror = () => {
        this.vivo = 'reconectando…';
      };
      this.es.onmessage = (m) => {
        try {
          this.snap = JSON.parse(m.data);
        } catch (_) {
          /* frame não-JSON (ping é comentário) */
        }
      };
    },

    // Verde do dot: MESMA regra de doctor::Veredito::ok (contrato) + o
    // fluxd são. Erro de rede no doctor = vermelho honesto.
    saudeDot() {
      if (this.saudeErro) return { c: 'err', t: 'doctor falhou' };
      if (!this.saude) return { c: 'idle', t: 'saúde…' };
      const tudo = this.saude.fluxd.ok && this.saude.projetos.every((p) => this.projetoOk(p));
      const n = this.saude.projetos.length;
      return tudo
        ? { c: 'ok pulse', t: 'fábrica ok · ' + n + '/' + n }
        : { c: 'err', t: 'atenção · veja os itens' };
    },

    projetoOk(p) {
      return p.containers.ok && p.sync.ok && p.banco.ok && p.assets.ok;
    },

    // ContainerState → (cor, rótulo) — espelho de formato::dot (contrato).
    dotDe(estado) {
      return (
        {
          awake: { c: 'ok pulse', t: 'acordado' },
          paused: { c: 'warn', t: 'pausado' },
          sleeping: { c: 'idle', t: 'dormindo' },
          starting: { c: 'warn', t: 'subindo' },
          failed: { c: 'err', t: 'falhou' },
        }[estado] || { c: 'idle', t: estado || '?' }
      );
    },

    // Epoch s → "há X"/"em X" — espelho de formato::quando.
    quandoRel(v) {
      if (!v) return '—';
      const diff = Math.round(v - Date.now() / 1000);
      const abs = Math.abs(diff);
      const pre = diff < 0 ? 'há ' : 'em ';
      if (abs < 60) return pre + abs + ' s';
      if (abs < 3600) return pre + Math.round(abs / 60) + ' min';
      return pre + Math.round(abs / 3600) + ' h';
    },

    resumo() {
      if (!this.snap) return 'carregando…';
      const n = this.snap.projetos.length;
      const acordados = this.snap.projetos.filter((p) => p.dev.estado === 'awake').length;
      const syncOk = this.snap.projetos.filter(
        (p) => p.sync && p.sync.problems === 0 && !p.sync.paused,
      ).length;
      return (
        n + ' projetos · ' + acordados + ' acordados · sync ' + syncOk + '/' + n +
        ' · vigia: ' + (this.snap.vigia_marca ? 'ativo' : 'sem ciclo ainda')
      );
    },

    // Curva 24h → barras (teto de DOM: reduz pra ≤96 buckets). Número puro,
    // nenhum dado do servidor vira HTML.
    barras() {
      const c = this.snap && this.snap.curva ? this.snap.curva.conts : [];
      if (!c.length) return [];
      const alvo = 96;
      const passo = Math.max(1, Math.ceil(c.length / alvo));
      const b = [];
      for (let i = 0; i < c.length; i += passo) {
        let soma = 0;
        for (let j = i; j < Math.min(i + passo, c.length); j++) soma += c[j];
        b.push(soma);
      }
      const max = Math.max(...b, 1);
      return b.map((v) => Math.max(2, Math.round((v / max) * 100)));
    },

    async acordar(nome) {
      this.acordando = { ...this.acordando, [nome]: true };
      try {
        const r = await fetch(
          '/api/projects/' + encodeURIComponent(nome) + '/acordar',
          { method: 'POST' },
        );
        if (!r.ok) {
          const d = await r.json().catch(() => ({}));
          this.aviso = 'acordar ' + nome + ' falhou: ' + (d.error || r.status);
        }
      } catch (e) {
        this.aviso = 'acordar ' + nome + ' falhou: ' + e.message;
      }
      this.acordando = { ...this.acordando, [nome]: false };
    },
  }));
});

// ------------------------------------------------------------ detalhe projeto
document.addEventListener('alpine:init', () => {
  Alpine.data('projetoPage', () => ({
    snap: null,
    erro: '',
    aviso: '',
    vivo: 'conectando…',
    es: null,
    logsLigado: false,
    logsEs: null,
    nome: decodeURIComponent(location.pathname.split('/').pop() || ''),

    init() {
      const q = new URLSearchParams(location.search);
      if (q.get('rescan') === '1') this.aviso = 'rescan na fila — o flux watch executa';
      else if (q.get('rescan') === 'falha') {
        this.aviso = 'rescan recusado — o motivo está nos eventos (escopo painel)';
      }
      this.conectar();
    },

    conectar() {
      this.es = new EventSource('/api/projects/' + encodeURIComponent(this.nome) + '/follow');
      this.es.onopen = () => {
        this.vivo = 'ao vivo';
      };
      this.es.onerror = () => {
        this.vivo = 'reconectando…';
      };
      this.es.onmessage = (m) => {
        try {
          const d = JSON.parse(m.data);
          if (d.erro) {
            this.erro = d.erro;
            return;
          }
          this.erro = '';
          this.snap = d;
        } catch (_) {}
      };
    },

    dotDe(estado) {
      return (
        {
          awake: { c: 'ok pulse', t: 'acordado' },
          paused: { c: 'warn', t: 'pausado' },
          sleeping: { c: 'idle', t: 'dormindo' },
          starting: { c: 'warn', t: 'subindo' },
          failed: { c: 'err', t: 'falhou' },
        }[estado] || { c: 'idle', t: estado || '?' }
      );
    },

    quandoRel(v) {
      if (!v) return '—';
      const diff = Math.round(v - Date.now() / 1000);
      const abs = Math.abs(diff);
      const pre = diff < 0 ? 'há ' : 'em ';
      if (abs < 60) return pre + abs + ' s';
      if (abs < 3600) return pre + Math.round(abs / 60) + ' min';
      return pre + Math.round(abs / 3600) + ' h';
    },

    // Tail de logs: MESMO endpoint da :7420 montado na API pública (SSE).
    // Linhas entram como TEXT (textContent) — log nunca vira HTML.
    ligarLogs() {
      if (this.logsLigado) return;
      this.logsLigado = true;
      const termo = this.$refs.logsterm;
      termo.textContent = '';
      const url =
        '/api/projects/' + encodeURIComponent(this.nome) + '/logs?env=dev&tail=200&follow=true';
      const es = new EventSource(url);
      this.logsEs = es;
      es.onmessage = (m) => {
        const linha = document.createElement('div');
        linha.textContent = m.data;
        termo.appendChild(linha);
        while (termo.childNodes.length > 500) termo.removeChild(termo.firstChild);
        termo.scrollTop = termo.scrollHeight;
      };
      es.onerror = () => {
        const aviso = document.createElement('div');
        aviso.className = 't-dim';
        aviso.textContent = '— stream caiu; reabrindo…';
        termo.appendChild(aviso);
      };
    },

    desligarLogs() {
      if (this.logsEs) this.logsEs.close();
      this.logsEs = null;
      this.logsLigado = false;
      this.$refs.logsterm.textContent = '';
    },
  }));
});
