/*! Painel flux — cliente do /events novo (fatia 4, 02/11/2026).
   Regras da casa, inegociáveis:
   - NUNCA injetar HTML com dado do servidor: mensagens entram só por
     x-text (textContent) — o XSS da era do swap de markup não volta.
     (Este arquivo não contém a API de injeção NEM a palavra: um teste
     do Rust garante.)
   - Hora LOCAL do navegador; UTC fica no `cru` (escopo decidido com o
     Gabriel).
   - Append no topo COM scroll compensado: nada desliza sob os olhos.
   - Agrupa repetidos em sequência (escopo+mensagem+nível) com contador ×N,
     SÓ dentro de 30 min de distância (03/11) — sequência que esfria não é
     "em sequência".
   Componentes registrados via Alpine.data (porta aberta pro build CSP). */

// ------------------------------------------------------------ formato (comum)
// Fonte única de formatação/decisão compartilhada pelas páginas — o servidor
// também tem um formato.rs; os contratos dos dois lados são espelhos.
const formato = {
  // MESMO formato de events.rs::linha_access — os dois lados têm de concordar.
  RE_ACCESS: /^(\S+) (\S+) (\d{3}) (\d+)ms host=([^\s]+)/,
  // IPs da própria infra (loopback, VM, host Windows do túnel): badge "host".
  IPS_INFRA: ['127.0.0.1', '::1', '192.168.140.1', '192.168.140.2'],
  // Janela máxima do agrupamento ×N (03/11).
  GAP_GRUPO_MS: 30 * 60 * 1000,

  // "YYYY-MM-DD HH:MM:SS" (UTC do banco) → Date local.
  data(utc) {
    return new Date(utc.replace(' ', 'T') + 'Z');
  },
  // Mesma data → hora LOCAL HH:MM:SS.
  quando(utc) {
    const d = this.data(utc);
    if (isNaN(d)) return utc;
    const p = (n) => String(n).padStart(2, '0');
    return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  },
  // Access da mensagem (null quando não é linha de access).
  parseAccess(msg) {
    const m = this.RE_ACCESS.exec(msg);
    return m
      ? { metodo: m[1], caminho: m[2], status: m[3], st: this.stClasse(+m[3]), dur: m[4], host: m[5] }
      : null;
  },
  // Mesmo mapeamento de formato.rs::classe_status (contrato).
  stClasse(s) {
    return s >= 500 || s === 429 ? 'st-err' : s >= 400 ? 'st-warn' : s >= 300 ? 'st-3' : 'st-ok';
  },
  dotDe(level) {
    const grave = level === 'warn' || level === 'error' || level === 'err';
    return level === 'warn'
      ? 'warn'
      : grave && level !== 'warn'
        ? 'err'
        : 'idle';
  },
  linhaDe(level) {
    const grave = level === 'warn' || level === 'error' || level === 'err';
    return level === 'warn' ? 'lwarn' : grave && level !== 'warn' ? 'lerr' : '';
  },
  // Marca de origem do evento (03/11): "você" = ip do NAVEGADOR aberto
  // (meuIp vem do snapshot, mesma régua ip_real do servidor); "host" = ip
  // da infra. null = sem marca (internet ou evento interno).
  marcaDe(ev, meuIp) {
    if (meuIp && ev.ip === meuIp) return { classe: 'voce', rotulo: 'você' };
    if (ev.ip && this.IPS_INFRA.includes(ev.ip)) return { classe: 'infra', rotulo: 'host' };
    return null;
  },
  // O evento merece sumir no modo "ocultar meus eventos"? SÓ info vinda do
  // meu IP (warn/error meu continuam visíveis — falha minha é sinal).
  eMeuInfo(ev, meuIp) {
    return !!meuIp && ev.ip === meuIp && ev.level === 'info';
  },
  // Link de investigação do evento: access → /events do host de origem;
  // sem host → busca pelo host da linha; interno → o escopo.
  linkDe(ev) {
    if (ev.host) return '/events?host=' + encodeURIComponent(ev.host);
    const a = this.parseAccess(ev.message);
    if (a) return '/events?q=' + encodeURIComponent(a.host);
    return '/events?escopo=' + encodeURIComponent(ev.scope);
  },
  // Anomalia do vigia → /events com a PISTA que o evento carrega: o ip do
  // ofensor (coluna ip) ou o caminho citado no texto ("caminho `/x`").
  linkAnomalia(ev) {
    if (ev.ip) return '/events?q=' + encodeURIComponent(ev.ip);
    const m = /caminho `([^`]+)`/.exec(ev.message);
    if (m) return '/events?q=' + encodeURIComponent(m[1]);
    return '/events?escopo=anomalia';
  },
  // Emissor (schema v8): tag "projeto" dos access do router — null nos
  // internos/antigos (coluna vazia). O clique abre o /events filtrado pela
  // coluna (?projeto=), com fallback de mensagem no servidor.
  tagProjeto(ev) {
    if (!ev.projeto) return null;
    return { classe: ev.env === 'dev' || ev.env === 'prod' ? ev.env : 'infra', rotulo: ev.projeto };
  },
  // Código de resposta (schema v8): coluna nos access/waf novos; fallback =
  // parse da mensagem (linhas antigas). null = sem código.
  stDe(ev) {
    if (ev.st != null) return { code: ev.st, classe: this.stClasse(ev.st) };
    const a = this.parseAccess(ev.message);
    return a ? { code: +a.status, classe: this.stClasse(+a.status) } : null;
  },
};

document.addEventListener('alpine:init', () => {
  Alpine.data('eventosPage', () => ({
    formato, // helpers compartilhados expostos pros templates
    escopos: [],
    escopo: '',
    nivel: '',
    busca: '',
    host: '',
    ambiente: '',
    projeto: '',
    hosts: [],
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
    meuIp: '', // IP real da request (mesma régua do servidor)
    ocultarMeus: false, // toggle: esconde os eventos de INFO do meu IP

    async init() {
      // URL vence o localStorage: os cliques do painel (anomalia, evento,
      // tag de projeto) chegam como ?escopo=/?host=/?projeto=/?q= — sem
      // isso o link caía SEM filtro (buraco do rework, 04/11).
      const q = new URLSearchParams(location.search);
      const salvo = JSON.parse(localStorage.getItem('flux.eventos.filtros') || '{}');
      const pega = (k, salvoK) => q.get(k) || salvo[salvoK] || '';
      this.escopo = pega('escopo', 'escopo');
      this.nivel = pega('nivel', 'nivel');
      this.busca = pega('q', 'busca');
      this.host = pega('host', 'host');
      this.ambiente = pega('ambiente', 'ambiente');
      this.projeto = pega('projeto', 'projeto');
      this.ocultarMeus = localStorage.getItem('flux.ocultar_meus') === '1';
      await this.recarregar();
      this.conectar();
      this.observer = new IntersectionObserver((entradas) => {
        if (entradas[0].isIntersecting) this.proximaPagina();
      });
      this.observer.observe(this.$refs.sentinel);
      setInterval(() => this.podaJanela(), 30000);
    },

    // Grupos visíveis com o toggle ligado: info VINDO DO MEU IP sai; warn e
    // erro meu continuam (falha minha é sinal, ruído meu é lixo).
    get visiveis() {
      if (!this.ocultarMeus || !this.meuIp) return this.eventos;
      return this.eventos.filter((g) => !formato.eMeuInfo(g, this.meuIp));
    },

    alternarMeus() {
      this.ocultarMeus = !this.ocultarMeus;
      localStorage.setItem('flux.ocultar_meus', this.ocultarMeus ? '1' : '0');
    },

    urlFiltros(extra) {
      const qs = new URLSearchParams();
      if (this.escopo) qs.set('escopo', this.escopo);
      if (this.nivel) qs.set('nivel', this.nivel);
      if (this.busca) qs.set('q', this.busca);
      if (this.host) qs.set('host', this.host);
      if (this.ambiente) qs.set('ambiente', this.ambiente);
      if (this.projeto) qs.set('projeto', this.projeto);
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
        this.hosts = d.hosts || [];
        this.meuIp = d.meu_ip || '';
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
        JSON.stringify({
          escopo: this.escopo,
          nivel: this.nivel,
          busca: this.busca,
          host: this.host,
          ambiente: this.ambiente,
          projeto: this.projeto,
        }),
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

    // Junta no grupo de cima quando escopo+mensagem+nível coincidem E a
    // distância pro anterior é ≤ 30 min (03/11): sequência que esfria vira
    // grupo novo — ×N não mente "em sequência".
    dobra(ev, aoVivo) {
      const g = this.grupoDe(ev);
      const topo = this.eventos[0];
      const noGap =
        topo &&
        topo.chave === g.chave &&
        formato.data(g.ultimo) - formato.data(topo.times[0]) <= formato.GAP_GRUPO_MS;
      if (noGap) {
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
      const g = {
        chave: ev.scope + '\u0000' + ev.message,
        message: ev.message,
        scope: ev.scope,
        level: ev.level,
        ip: ev.ip || '',
        host: ev.host || '',
        projeto: ev.projeto || '',
        env: ev.env || '',
        st: ev.st == null ? null : ev.st,
        times: [ev.created_at],
        ultimo: ev.created_at,
        count: 1,
        dot: formato.dotDe(ev.level),
        linha: formato.linhaDe(ev.level),
        access: formato.parseAccess(ev.message),
      };
      return g;
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
          // DESC: apenda no fim; dobra com a calda se a sequência continua
          // (mesma régua de 30 min — o tempo MAIS ANTIGO da calda é quem
          // encosta no evento mais velho que está chegando).
          const g = this.grupoDe(ev);
          const calda = this.eventos[this.eventos.length - 1];
          const maisVelho = calda ? calda.times[calda.times.length - 1] : null;
          const noGap =
            maisVelho &&
            calda.chave === g.chave &&
            formato.data(maisVelho) - formato.data(g.ultimo) <= formato.GAP_GRUPO_MS;
          if (noGap) {
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
      const m = formato.parseAccess(ev.message);
      if (!m) return;
      this.janela.push({ t: Date.now() / 1000, st: +m.status });
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
      return formato.quando(utc);
    },
  }));
});

// ------------------------------------------------------------------ dashboard
document.addEventListener('alpine:init', () => {
  // Instância do Chart da curva FORA do estado reativo do Alpine (doc no
  // desenharCurva). Observe usa destroy+recreate e escapa do loop; aqui o
  // update é por SET de .data — pelo proxy do Alpine isso era recursão.
  let curvaChart = null;
  Alpine.data('dashboardPage', () => ({
    formato, // helpers compartilhados expostos pros templates
    snap: null,
    saude: null,
    saudeErro: '',
    saudeAberto: false,
    limiaresAberto: false,
    limiares: null,
    novoLimiarProjeto: '',
    novoLimiarBase: '',
    aviso: '',
    vivo: 'conectando…',
    acordando: {},
    es: null,
    // Curva (03/11): janela em minutos + realtime (últimos 10 min via SSE —
    // o servidor coalesce os pushes em ≥300 ms). A instância do Chart NÃO
    // mora aqui: ver curvaChart no escopo do módulo, logo acima.
    janela: 1440,
    realtime: false,
    JANELAS: [1440, 720, 360, 60, 30, 15],
    valorSelecionado: null,
    // Projetos: 6 por página (03/11) — paginação no cliente, lista vem do
    // snapshot.
    porPagina: 6,
    paginaProjetos: 1,
    // Toggle "ocultar meus eventos" — MESMA chave do /events (03/11).
    ocultarMeus: false,

    init() {
      const q = new URLSearchParams(location.search);
      if (q.get('rescan') === '1') this.aviso = 'rescan na fila — o flux watch executa';
      else if (q.get('rescan') === 'falha') {
        this.aviso = 'rescan recusado — o motivo está nos eventos (escopo painel)';
      }
      this.ocultarMeus = localStorage.getItem('flux.ocultar_meus') === '1';
      this.puxaSaude();
      setInterval(() => this.puxaSaude(), 60000);
      this.puxaLimiares();
      this.conectar();
      // Tema do SO mudou → gráfico redesenha com as cores novas.
      window
        .matchMedia('(prefers-color-scheme: dark)')
        .addEventListener('change', () => this.desenharCurva());
    },

    get meuIp() {
      return (this.snap && this.snap.meu_ip) || '';
    },

    // "Últimos eventos" com o toggle ligado: info VINDO DO MEU IP sai
    // (mesma regra do /events, MESMA chave de localStorage).
    get visiveis() {
      const lista = (this.snap && this.snap.eventos) || [];
      if (!this.ocultarMeus || !this.meuIp) return lista;
      return lista.filter((ev) => !formato.eMeuInfo(ev, this.meuIp));
    },

    alternarMeus() {
      this.ocultarMeus = !this.ocultarMeus;
      localStorage.setItem('flux.ocultar_meus', this.ocultarMeus ? '1' : '0');
    },

    // ------------------------------------------------------------ curva
    // Janela mudou: busca o snapshot já na janela e REABRE o follow com o
    // ?minutos= novo (o hash do push é por janela — fonte única no servidor).
    async mudarJanela(v) {
      this.valorSelecionado = null;
      if (v === 'rt') {
        this.realtime = true;
        this.janela = 10;
      } else {
        this.realtime = false;
        this.janela = v;
      }
      await this.puxar();
      this.conectar();
    },

    async puxar() {
      try {
        const r = await fetch('/api/dashboard?minutos=' + this.janela);
        if (!r.ok) throw new Error('api ' + r.status);
        this.aplicarSnap(await r.json());
      } catch (e) {
        this.vivo = 'falhou: ' + e.message;
      }
    },

    aplicarSnap(snap) {
      this.snap = snap;
      this.$nextTick(() => this.desenharCurva());
    },

    // Min/max/média POR MINUTO da janela (03/11) + onde está o pico.
    stats() {
      const c = this.snap && this.snap.curva ? this.snap.curva.conts : [];
      if (!c.length) return null;
      const max = Math.max(...c);
      const media = c.reduce((a, b) => a + b, 0) / c.length;
      return {
        min: Math.min(...c),
        max,
        media: Math.round(media * 10) / 10,
        maxIdx: c.indexOf(max),
      };
    },

    // Hora local do bucket i (o ÚLTIMO bucket é o minuto corrente).
    quandoBucket(i) {
      const c = this.snap && this.snap.curva ? this.snap.curva.conts : [];
      const agora = Math.floor(Date.now() / 60000) * 60000;
      const d = new Date(agora - (c.length - 1 - i) * 60000);
      const p = (n) => String(n).padStart(2, '0');
      return p(d.getHours()) + ':' + p(d.getMinutes());
    },

    // Gráfico da curva (Chart.js vendido, MESMO padrão do observe): linha de
    // eventos/min; CLIQUE no ponto fixa o valor selecionado (03/11).
    // A instância vive em curvaChart (módulo, FORA do estado reativo):
    // guardada no componente, o Alpine proxy-wraps o Chart e o SET
    // `chart.data = …` do update entra em loop com o getter interno do
    // próprio Chart — "too much recursion", console do Gabriel 03/11.
    desenharCurva() {
      if (!this.snap || !this.snap.curva || typeof Chart === 'undefined') return;
      const cor = {
        linha: this.corCss('--info') || '#58a6ff',
        grade: this.corCss('--line') || '#262d37',
        texto: this.corCss('--muted') || '#8b98a7',
      };
      const conts = this.snap.curva.conts;
      const agora = Math.floor(Date.now() / 60000) * 60000;
      const pontos = conts.map((v, i) => ({ x: agora - (conts.length - 1 - i) * 60000, y: v }));
      const self = this;
      if (curvaChart) {
        curvaChart.data.datasets[0].data = pontos;
        curvaChart.update('none');
        return;
      }
      curvaChart = new Chart(this.$refs.curva.getContext('2d'), {
        type: 'line',
        data: {
          datasets: [
            {
              label: 'eventos/min',
              data: pontos,
              borderColor: cor.linha,
              backgroundColor: cor.linha,
              pointRadius: 0,
              borderWidth: 1.5,
              tension: 0.25,
            },
          ],
        },
        options: {
          responsive: true,
          animation: false,
          maintainAspectRatio: false,
          interaction: { mode: 'nearest', intersect: false },
          onClick(_evt, els) {
            if (!els.length) return;
            const p = pontos[els[0].index];
            self.valorSelecionado = {
              quando: self.quandoBucket(els[0].index),
              valor: p.y,
              pico: self.stats() ? p.y === self.stats().max : false,
            };
          },
          plugins: {
            legend: { display: false },
            tooltip: {
              callbacks: {
                title: (itens) =>
                  itens.length ? new Date(itens[0].parsed.x).toLocaleTimeString() : '',
              },
            },
          },
          scales: {
            x: {
              type: 'linear',
              ticks: {
                color: cor.texto,
                maxTicksLimit: 6,
                callback: (v) => new Date(v).toLocaleTimeString(),
              },
              grid: { color: cor.grade },
            },
            y: {
              beginAtZero: true,
              ticks: { color: cor.texto, precision: 0 },
              grid: { color: cor.grade },
            },
          },
        },
      });
    },

    corCss(nome) {
      return getComputedStyle(document.documentElement).getPropertyValue(nome).trim();
    },

    // ------------------------------------------------------------ vigia
    // Idade do último ciclo em s (null = nunca rodou). O ciclo é de 60 s:
    // ≤2 min saudável, até 10 min desconfiado, mais que isso = morto.
    vigiaIdade() {
      if (!this.snap || !this.snap.vigia_marca) return null;
      return Math.max(0, Math.round(Date.now() / 1000 - parseInt(this.snap.vigia_marca, 10)));
    },
    vigiaDot() {
      const s = this.vigiaIdade();
      if (s === null) return 'idle';
      return s <= 120 ? 'ok pulse' : s <= 600 ? 'warn' : 'err';
    },
    vigiaTexto() {
      const s = this.vigiaIdade();
      if (s === null) return 'nunca rodou';
      const p = (n) => String(n).padStart(2, '0');
      const d = new Date(parseInt(this.snap.vigia_marca, 10) * 1000);
      const rel =
        s < 60 ? s + ' s' : s < 3600 ? Math.round(s / 60) + ' min' : Math.round(s / 3600) + ' h';
      return 'há ' + rel + ' (' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds()) + ' local)';
    },

    // ------------------------------------------------------------ projetos
    paginasProjetos() {
      const n = this.snap ? this.snap.projetos.length : 0;
      return Math.max(1, Math.ceil(n / this.porPagina));
    },
    projetosPagina() {
      if (!this.snap) return [];
      const ini = (this.paginaProjetos - 1) * this.porPagina;
      return this.snap.projetos.slice(ini, ini + this.porPagina);
    },
    mudaPaginaProjetos(d) {
      const novo = this.paginaProjetos + d;
      if (novo >= 1 && novo <= this.paginasProjetos()) this.paginaProjetos = novo;
    },

    // Dot dos itens do doctor: NEUTRO (projeto que nunca subiu) é CINZA —
    // verde pareceria "rodando" e vermelho alarmaria à toa (03/11).
    classeItem(it) {
      if (!it) return 'idle';
      return it.neutro ? 'idle' : it.ok ? 'ok' : 'err';
    },

    conectar() {
      if (this.es) this.es.close();
      this.es = new EventSource('/api/dashboard/follow?minutos=' + this.janela);
      this.es.onopen = () => {
        this.vivo = 'ao vivo';
      };
      this.es.onerror = () => {
        this.vivo = 'reconectando…';
      };
      this.es.onmessage = (m) => {
        try {
          this.aplicarSnap(JSON.parse(m.data));
        } catch (_) {
          /* frame não-JSON (ping é comentário) */
        }
      };
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

    async puxaLimiares() {
      try {
        const r = await fetch('/api/vigia/limiares');
        if (r.ok) this.limiares = await r.json();
      } catch (_) {}
    },

    async salvarLimiar() {
      const projeto = this.novoLimiarProjeto.trim();
      const base = parseInt(this.novoLimiarBase, 10);
      if (!projeto || !base) return;
      try {
        const r = await fetch('/api/vigia/limiares', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ projeto, base }),
        });
        const d = await r.json().catch(() => ({}));
        if (!r.ok) {
          this.aviso = 'limiar recusado: ' + (d.error || r.status);
          return;
        }
        this.aviso = 'limiar do vigia: ' + d.resultado + ' (vale em ≤ 60 s)';
        this.novoLimiarProjeto = '';
        this.novoLimiarBase = '';
        this.puxaLimiares();
      } catch (e) {
        this.aviso = 'limiar falhou: ' + e.message;
      }
    },

    async removerLimiar(projeto) {
      try {
        await fetch('/api/vigia/limiares', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ projeto, remover: true, base: 0 }),
        });
        this.puxaLimiares();
      } catch (_) {}
    },

    // conectar() (com ?minutos=) vive no bloco de cima — declarado DUAS vezes
    // num literal de objeto a última vence e matava a janela da curva.

    // Verde do dot: MESMA regra de doctor::Veredito::ok (contrato) + o
    // fluxd são. Erro de rede no doctor = vermelho honesto. Projeto que
    // nunca subiu é neutro — não segura o verde, mas aparece na contagem.
    saudeDot() {
      if (this.saudeErro) return { c: 'err', t: 'doctor falhou' };
      if (!this.saude) return { c: 'idle', t: 'saúde…' };
      const tudo = this.saude.fluxd.ok && this.saude.projetos.every((p) => this.projetoOk(p));
      const n = this.saude.projetos.length;
      const nunca = this.nNuncaSubiu();
      const espera = nunca > 0 ? ' · ' + nunca + ' aguardando 1º up' : '';
      return tudo
        ? { c: 'ok pulse', t: 'fábrica ok · ' + n + '/' + n + espera }
        : { c: 'err', t: 'atenção · veja os itens' };
    },

    // Projetos que nunca rodaram `flux up` (doctor: containers nunca-subiu).
    nNuncaSubiu() {
      if (!this.saude) return 0;
      return this.saude.projetos.filter((p) => p.containers.estado === 'nunca-subiu').length;
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

    // Curva 24h → BARRAS removidas (03/11): a curva é Chart.js (desenharCurva)
    // com janelas e realtime — o sparkline de divs não volta.

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

// ------------------------------------------------------------- projetos (03/11)
document.addEventListener('alpine:init', () => {
  Alpine.data('projetosPage', () => ({
    formato,
    snap: null,
    vivo: 'conectando…',
    aviso: '',
    es: null,
    porPagina: 6,
    pagina: 1,
    acordando: {},

    init() {
      const q = new URLSearchParams(location.search);
      if (q.get('rescan') === '1') this.aviso = 'rescan na fila — o flux watch executa';
      else if (q.get('rescan') === 'falha') {
        this.aviso = 'rescan recusado — o motivo está nos eventos (escopo painel)';
      }
      this.conectar();
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

    // MESMAS funções dos cards do dashboardPage — os cards são o MESMO bloco
    // nas duas shells (teste Rust garante a paridade das bindings).
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

    paginas() {
      const n = this.snap ? this.snap.projetos.length : 0;
      return Math.max(1, Math.ceil(n / this.porPagina));
    },
    paginaAtual() {
      if (!this.snap) return [];
      const ini = (this.pagina - 1) * this.porPagina;
      return this.snap.projetos.slice(ini, ini + this.porPagina);
    },
    muda(d) {
      const novo = this.pagina + d;
      if (novo >= 1 && novo <= this.paginas()) this.pagina = novo;
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
    acoes: [
      { id: 'migrate', rotulo: 'migrate', destrutiva: false, prod: true },
      { id: 'optimize', rotulo: 'optimize:clear', destrutiva: false, prod: true },
      { id: 'shield', rotulo: 'shield:generate', destrutiva: false, prod: true },
      { id: 'build', rotulo: 'build (dev)', destrutiva: false, prod: false },
      { id: 'seed', rotulo: 'seed (DEV)', destrutiva: true, prod: false },
      { id: 'fresh', rotulo: 'migrate:fresh (DEV)', destrutiva: true, prod: false },
      { id: 'fresh_seed', rotulo: 'fresh+seed (DEV)', destrutiva: true, prod: false },
    ],
    acaoAtual: null,
    streamOn: false,
    acaoEs: null,

    init() {
      const q = new URLSearchParams(location.search);
      if (q.get('rescan') === '1') this.aviso = 'rescan na fila — o flux watch executa';
      else if (q.get('rescan') === 'falha') {
        this.aviso = 'rescan recusado — o motivo está nos eventos (escopo painel)';
      }
      this.conectar();
      this.puxarAcao();
    },

    // ---------- ações (fatia 6b)
    async puxarAcao() {
      try {
        const r = await fetch('/api/projects/' + encodeURIComponent(this.nome) + '/acoes/atual');
        if (r.ok) {
          this.acaoAtual = await r.json();
          if (this.acaoAtual.estado === 'rodando') this.seguirAcao();
        }
      } catch (_) {}
    },

    clicarAcao(a) {
      if (a.destrutiva && !a.confirmando) {
        // DOIS CLIQUES: arma a confirmação; desarma em 5 s.
        a.confirmando = true;
        setTimeout(() => { a.confirmando = false; }, 5000);
        return;
      }
      a.confirmando = false;
      this.acao(a.id);
    },

    async acao(id) {
      try {
        const r = await fetch('/api/projects/' + encodeURIComponent(this.nome) + '/acoes', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ acao: id, ambiente: 'dev' }),
        });
        const d = await r.json().catch(() => ({}));
        if (!r.ok) {
          this.aviso = id + ' recusado: ' + (d.error || r.status);
          return;
        }
        this.acaoAtual = d;
        this.seguirAcao();
      } catch (e) {
        this.aviso = id + ' falhou: ' + e.message;
      }
    },

    seguirAcao() {
      if (this.streamOn) return;
      this.streamOn = true;
      const termo = this.$refs.termo;
      termo.textContent = '';
      const es = new EventSource(
        '/api/projects/' + encodeURIComponent(this.nome) + '/acoes/stream',
      );
      this.acaoEs = es;
      es.addEventListener('fim', () => {
        es.close();
        this.streamOn = false;
        this.puxarAcao();
      });
      es.onmessage = (m) => {
        const linha = document.createElement('div');
        linha.textContent = m.data;
        termo.appendChild(linha);
        while (termo.childNodes.length > 400) termo.removeChild(termo.firstChild);
        termo.scrollTop = termo.scrollHeight;
        this.acaoAtual && (this.acaoAtual.saida = this.acaoAtual.saida || []);
      };
      es.onerror = () => {
        es.close();
        this.streamOn = false;
      };
    },

    liveDot() {
      const l = this.snap && this.snap.live;
      if (!l) return { c: 'idle', t: 'sem como medir' };
      return l.hot && l.porta
        ? { c: 'ok pulse', t: 'live ON' }
        : l.hot
          ? { c: 'warn', t: 'hot órfão (porta morta)' }
          : { c: 'idle', t: 'live OFF' };
    },

    liveTexto() {
      const l = this.snap && this.snap.live;
      return l ? 'hot=' + l.hot + ' · porta 5173=' + l.porta : '(sem engine ou sem IP no state)';
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

// ------------------------------------------------------------------ observe
document.addEventListener('alpine:init', () => {
  Alpine.data('observePage', () => ({
    snap: null,
    projeto: '',
    container: '',
    metrica: 'cpu',
    minutos: 1440,
    projetos: [],
    vivo: 'conectando…',
    es: null,
    graficos: {}, // canvas → Chart (destrói antes de recriar)

    ROTULOS: {
      cpu: 'CPU (%)',
      mem: 'Memória (%)',
      net_rx: 'Rede RX',
      net_tx: 'Rede TX',
    },

    init() {
      const q = new URLSearchParams(location.search);
      const p = q.get('projeto');
      const c = q.get('container');
      const m = q.get('metrica');
      const mm = parseInt(q.get('minutos'), 10);
      if (p) this.projeto = p;
      if (c) this.container = c;
      if (m && this.ROTULOS[m]) this.metrica = m;
      if ([60, 360, 1440].includes(mm)) this.minutos = mm;
      this.recarregar();
      this.conectar();
      // Tema do SO mudou → gráficos redesenham com as cores novas.
      window
        .matchMedia('(prefers-color-scheme: dark)')
        .addEventListener('change', () => this.desenhar());
    },

    url() {
      const qs = new URLSearchParams();
      if (this.projeto) qs.set('projeto', this.projeto);
      if (this.container) qs.set('container', this.container);
      qs.set('metrica', this.metrica);
      qs.set('minutos', this.minutos);
      return '/api/observe?' + qs.toString();
    },

    async recarregar() {
      try {
        const r = await fetch(this.url());
        if (!r.ok) throw new Error('api ' + r.status);
        const d = await r.json();
        this.snap = d;
        this.projetos = d.projetos || [];
        this.$nextTick(() => this.desenhar());
      } catch (e) {
        this.vivo = 'falhou: ' + e.message;
      }
    },

    conectar() {
      this.es = new EventSource('/api/observe/follow?' + this.url().split('?')[1]);
      this.es.onopen = () => {
        this.vivo = 'ao vivo';
      };
      this.es.onerror = () => {
        this.vivo = 'reconectando…';
      };
      this.es.onmessage = (m) => {
        try {
          this.snap = JSON.parse(m.data);
          this.projetos = this.snap.projetos || [];
          this.$nextTick(() => this.desenhar());
        } catch (_) {}
      };
    },

    // Cores do style guide lidas do CSS (tema claro/escuro de graça).
    cor(nome) {
      return getComputedStyle(document.documentElement).getPropertyValue(nome).trim();
    },

    destruirGraficos() {
      for (const k of Object.keys(this.graficos)) {
        this.graficos[k].destroy();
        delete this.graficos[k];
      }
    },

    desenhar() {
      if (!this.snap || typeof Chart === 'undefined') return;
      this.destruirGraficos();
      const cor = {
        linha: this.cor('--info') || '#58a6ff',
        pico: this.cor('--err') || '#f85149',
        grade: this.cor('--line') || '#262d37',
        texto: this.cor('--muted') || '#8b98a7',
      };
      const fmtValor = (v) => (v >= 100 ? Math.round(v) : v.toFixed(1));
      const base = {
        borderColor: cor.linha,
        backgroundColor: cor.linha,
        pointRadius: 0,
        tension: 0.25,
        borderWidth: 1.5,
      };
      const opcoes = (rotuloY) => ({
        responsive: true,
        animation: false,
        interaction: { mode: 'nearest', intersect: false },
        plugins: {
          legend: { display: false },
          tooltip: {
            callbacks: {
              title: (itens) =>
                itens.length
                  ? new Date(itens[0].parsed.x).toLocaleTimeString()
                  : '',
            },
          },
        },
        scales: {
          x: {
            type: 'linear',
            ticks: { color: cor.texto, maxTicksLimit: 6, callback: (v) => new Date(v).toLocaleTimeString() },
            grid: { color: cor.grade },
          },
          y: {
            ticks: { color: cor.texto, callback: (v) => fmtValor(v) },
            grid: { color: cor.grade },
            title: { display: !!rotuloY, text: rotuloY || '', color: cor.texto },
          },
        },
      });
      const datasetSerie = {
        label: 'série',
        data: this.snap.serie.map(([t, v]) => ({ x: t, y: v })),
        ...base,
      };
      // Picos MARCADOS na curva (decisão de escopo): scatter vermelho nos
      // instantes em que o vigia capturou pico DA MESMA métrica.
      const picosDaMetrica = this.snap.picos.filter((p) =>
        this.metricaComeca(p.metric, this.metrica),
      );
      const datasetPicos = {
        label: 'pico',
        data: picosDaMetrica.map((p) => ({ x: p.ts * 1000, y: p.valor })),
        pointRadius: 5,
        pointBackgroundColor: cor.pico,
        borderColor: 'transparent',
        showLine: false,
      };
      const ctxCurva = this.$refs.curva.getContext('2d');
      this.graficos.curva = new Chart(ctxCurva, {
        type: 'line',
        data: { datasets: [datasetSerie, datasetPicos] },
        options: opcoes(''),
      });
      // Mini-curvas por container.
      for (const c of this.snap.por_container) {
        const el = document.getElementById('mini-' + c.container);
        if (!el) continue;
        this.graficos[c.container] = new Chart(el.getContext('2d'), {
          type: 'line',
          data: {
            datasets: [
              {
                label: c.container,
                data: c.serie.map(([t, v]) => ({ x: t, y: v })),
                ...base,
              },
            ],
          },
          options: {
            ...opcoes(''),
            plugins: { legend: { display: false }, tooltip: { enabled: false } },
            scales: {
              x: { display: false, type: 'linear' },
              y: { display: false },
            },
          },
        });
      }
    },

    // Picos guardam a métrica como coluna ('cpu_pct'/'mem_pct'/...): casa
    // pelo prefixo — MESMO contrato do SQL do observe.
    metricaComeca(metricaPico, filtro) {
      return metricaPico.startsWith(filtro.split('_')[0]);
    },

    rotuloMetrica(m) {
      return this.ROTULOS[m] || m;
    },
    rotuloJanela() {
      return this.minutos === 60 ? '1 h' : this.minutos === 360 ? '6 h' : '24 h';
    },
    escopoTexto() {
      return this.container
        ? 'container ' + this.container
        : this.projeto
          ? 'projeto ' + this.projeto
          : 'global';
    },
    resumo() {
      if (!this.snap) return 'carregando…';
      return (
        this.rotuloMetrica(this.metrica) + ' · ' + this.escopoTexto() + ' · ' + this.rotuloJanela()
      );
    },
  }));
});
