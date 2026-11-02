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
