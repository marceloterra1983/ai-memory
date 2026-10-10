# Instalação e rollback da entrada confiável

Isto é o procedimento para o root **depois da revisão**. `reliable-mcp
install` não executa estes passos. Não copia token, não muda a unit base
e não liga autenticação nova nem desliga a que já existe.

Os clientes permanecem em `http://127.0.0.1:49374/mcp`. Só o bind do
backend muda, por drop-in, para `127.0.0.1:49376`.

## Stage

Numa cópia da unit que está em execução (não editar a unit base):

```text
reliable-mcp install --stage /tmp/reliable-mcp-stage --unit /caminho/ai-memory.service
```

O stage contém:

- `units/ai-memory.service.d/backend-bind.conf` — `ExecStart=` vazio e um
  `ExecStart` igual ao da unit, com o bind trocado para `127.0.0.1:49376`.
  Linhas `Environment=` não são copiadas.
- `units/ai-memory-ingress.service` e `units/ai-memory-delivery.service`.
  Não têm `PartOf=`, `Requires=` nem `BindsTo=` com o backend. Arrancam com
  `WantedBy=default.target` e `After=network-online.target`.
- `source/` — lançador e pacote Python, para instalar em
  `~/.agents/tools/ai-memory/reliable-mcp/`. Não se corre a partir do worktree.
- `patches/install-ai-memory-ready.patch` — troca o bloco `ready=0` do
  instalador para chamar `ready --backend`. O par vem de
  `manifest.json` `service.readiness_scope` (`workspace` e `project`).
  Override só quando `AI_MEMORY_READY_WORKSPACE` e
  `AI_MEMORY_READY_PROJECT` vêm os dois, não vazios. Um só é erro.
  Sem manifest e sem o par completo, o instalador para com erro claro.
  Não infere o último projeto. `--check` continua só leitura.

Sem `--stage`, o comando só imprime o plano e diz «não instala».

## Ordem do cutover

1. Copiar `source/` para `~/.agents/tools/ai-memory/reliable-mcp/` com
   dono do usuário e diretórios `0700` / arquivos `0600` (o lançador `0755`).
2. `bootstrap` contra o backend **ainda** em `:49374`. Se `create_only` não
   aparecer no `tools/list`, abortar. Não mover a porta.
3. Criar a outbox `~/.local/share/ai-memory-delivery` (`0700`), fora de
   `~/.local/share/ai-memory`.
4. Parar `ai-memory.service`.
5. Instalar só o drop-in em
   `~/.config/systemd/user/ai-memory.service.d/backend-bind.conf`.
   Não editar a unit base.
6. `systemctl --user daemon-reload`.
7. Instalar e arrancar `ai-memory-ingress.service` e
   `ai-memory-delivery.service` (usuário). O ingress escuta `:49374`. O
   backend, ao voltar, escuta `:49376` pelo drop-in.
8. `systemctl --user start ai-memory.service`.
9. Prontidão contra o backend, não contra o ingress:

```text
~/.agents/tools/ai-memory/reliable-mcp/reliable-mcp ready \
  --backend http://127.0.0.1:49376 --workspace WORKSPACE --project PROJECT
```

O par é o `service.readiness_scope` do manifest (nesta máquina o operador
confirmou `workspace=default` e `project=dev`). Para um corte pontual,
exporte os dois `AI_MEMORY_READY_*` juntos. O par tem de ser um escopo que
já existe. Não adivinhar pelo último projeto do servidor.

## Rollback

1. Parar `ai-memory-ingress.service` e `ai-memory-delivery.service`.
2. Remover o drop-in `backend-bind.conf`.
3. `systemctl --user daemon-reload`.
4. Reiniciar a unit base. O bind volta ao da unit, `:49374`.
5. Deixar a outbox no lugar. Não apagar intenções pendentes ou em conflito.

## O que não muda

Política, scheduler, dream, dados e credenciais reais. Nenhuma exposição
nova: listen e backend são loopback, e listen não pode ser igual ao
backend. Hooks e o resto do HTTP são encaminhados. O cache de bootstrap
não substitui a prontidão.
