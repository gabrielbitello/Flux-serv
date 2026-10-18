# 02 — Sync de arquivos Windows ↔ VM

## Objetivo

O código mora no Windows (o Gabriel edita no working tree local), mas roda na VM.
A árvore `/srv/flux/projects/{name}` tem que refletir o editor em segundos, sem
prompt, sem comando manual, sem apagar artefato gerado dentro da VM.

## Mecanismo

### Motor: mutagen, modo `one-way-replica`

- Windows = **alpha** (fonte da verdade), VM = **beta** (réplica exata). Igual ao
  modelo rsync `--delete` do devfactory atual, mas com watcher nativo dos dois
  lados (latência típica < 1 s em LAN; pior caso < 5 s).
- O CLI mantém **uma sessão mutagen por projeto** (`mutagen sync create
  --name=flux-{name} --label=flux`), endpoint remoto `flux@192.168.140.2:/srv/flux/projects/{name}`.
  O agente mutagen no lado VM é copiado e iniciado automaticamente pelo próprio
  mutagen via SSH — nada a instalar lá.
- `flux sync status` mostra estado, latência e pendências; o painel mostra o mesmo.

### Branches: um diretório na VM por worktree

A URL de dev reflete o que está aberto no Windows — e branches distintas
abrem ao mesmo tempo (requisito R15):

- **Branch atual do projeto** → `{name}-d.bitello.xyz` serve
  `/srv/flux/projects/{name}` (a pasta sincronizada normal).
- **Branch em worktree** (ex.: `git worktree add ../vippers-api2 api2`) →
  `api2-vippers-d.bitello.xyz` serve `/srv/flux/projects/{name}@{branch}`,
  com sessão mutagen própria apontando para a pasta do worktree no Windows.
  Um worktree por branch = branches simultâneas de verdade, cada uma com seu
  container dev e banco `{name}_{branch-slug}_dev`.
- O CLI descobre branches ao vivo: watcher leve `flux watch` (serviço opcional
  no Windows) monitora `git worktree list` + branch atual das pastas
  registradas e reporta ao `fluxd` em mudança (debounce 2 s). Sem o watcher,
  `flux up` atualiza no ato.
- **Branch que só existe no GitHub**: host responde 200 com página brandiada
  "branch `api2` não encontrada localmente — `git worktree add` para usar",
  nada quebra. Nunca 404 cru (03).
- Nome de branch com `/` ou caracteres fora de `[a-z0-9-]` é sanitizado
  (`/` → `-`) no hostname; a URL derivada é determinística.

### Excludes (os dois lados respeitam)

Padrão global (não configurável por segurança/estabilidade):

```
.env
.env.*
public/hot
vendor/
node_modules/
public/build/
storage/
```

> **Ajuste 18/10/2026 (commit `f29ebfc`)**: `public/fonts/`, `public/css/` e
> `public/js/` saíram da lista. A premissa "tudo ali é artefato de build"
> quebrou no vippers: esses diretórios também hospedam asset PUBLICADO
> commitado no repo (60 arquivos de `filament:assets` + `js/app/
> money-script.js`) — asset trackeado pelo git é fonte da verdade e TEM que
> sincronizar; o artefato do vite vive em `public/build/`, que segue
> ignorado. Ignorado ≠ commitado. Sessões vivas só pegam a lista nova
> recriadas (`mutagen sync terminate` + `flux up`).

- `respectGitignore = true` (o `.gitignore` do projeto entra de graça).
- Projeto pode **adicionar** excludes em `flux.toml` (`[sync] extra_ignore`),
  nunca remover os globais.
- `.env` fica EXCLUSIVO do lado VM: `flux env pull` copia para o Windows como
  `.env.flux` (gitignored), o Gabriel edita e `flux env push` devolve. Assim o
  editor não sobrescreve segredo por acidente e o rsync nunca carrega `.env`.
- Diretórios excluídos precisam EXISTIR no beta: o provisionamento do projeto
  (`fluxd`) cria skeleton (`storage/framework/{cache,sessions,views}` etc.) no
  primeiro up, não o sync.

### Propagação de delete

`one-way-replica` propaga deleções (arquivo apagado no Windows desaparece na VM).
É o comportamento desejado (branch switch limpa rastro), mas é exatamente o que
torna proibido rodar qualquer OUTRA ferramenta de escrita em massa no beta
(anochega com a réplica e apaga código).

### Edição em andamento (importante para migrations)

mutagen agrupa mudanças; para arquivos críticos (`database/migrations/*`) o
`fluxd` aplica uma segunda trava no lado VM: inotify + janela de silêncio de
5 s + leitura dupla com hash igual antes de declarar "migration estável".
Detalhe em [05](05-banco-unico-migrations.md).

### Controles

| Comando | Efeito |
|---|---|
| `flux sync status` | estado da sessão, latência, arquivos pendentes, conflitos (não devem existir em one-way) |
| `flux sync pause` / `resume` | pausa a sync (ex.: operação destrutiva na VM) — banner no painel enquanto pausada |
| `flux sync reset` | mata sessão e recria com full rescan (termina sozinho, mostra progresso) |
| `flux env pull` / `push` / `edit` | ciclo do `.env` exclusivo da VM |

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| mutagen `one-way-replica` | engine nativa Rust (notify + diff) | Engine de sync correta (ordenação de renames, escrita parcial, atômicos) é um projeto em si; mutagen é maduro, OSS e zero daemon extra na VM (agente injetado) |
| mutagen `one-way-replica` | `two-way-resolved` (bidirecional) | Bidirecional cria conflito e sobrescreve artefato gerado na VM; modelo mental atual (Windows = fonte) é provado em produção no devfactory |
| `.env` fora do sync | sincronizar `.env` | Segredo não circula; push/pull explícitos com diff |
| Samba/9p mount | — | Bind mount de FS de rede dentro do Docker = performance péssima (vendor/node_modules) e travamentos de oplock |

## O que NUNCA fazer

- **Nunca** rodar `rsync --delete` manual para o mesmo par de diretórios de uma
  sessão mutagen ativa — corrida de escrita e deleção cruzada.
- **Nunca** sincronizar `storage/`, `vendor/`, `node_modules/`, artefatos de build.
  É o que protege o container de perder estado a cada sync.
- **Nunca** escrever no beta (VM) por fora — incluindo "só rapidinho" via
  `flux vm ssh`. Mudança de emergência: aplica no Windows e deixa a sync levar
  (< 5 s), ou pausa a sync primeiro e anuncia no painel.
- **Nunca** apagar a pasta do projeto na VM sem `flux sync reset` depois.

## Critérios de aceite

1. Editar arquivo no Windows → conteúdo novo em `/srv/flux/projects/{name}` em
   < 5 s (medição com `date` nos dois lados, 10 amostras, regime estável).
2. Criar arquivo → aparece; apagar → some; renomear → não duplica.
3. `.env` editado no Windows NUNCA aparece na VM; `flux env push` reflete na
   próxima sync na forma de arquivo allowlist explícito.
4. Trocar de branch com migrations novas dispara o pipeline correto (05).
5. `flux sync status` converge para `ok · sem pendências` após qualquer rajada.
6. Worktree `api2` do projeto vippers → `api2-vippers-d.bitello.xyz` reflete a
   pasta do worktree em < 5 s, banco próprio, container próprio.
7. Host de branch inexistente localmente → página brandiada com instrução,
   HTTP 200.
