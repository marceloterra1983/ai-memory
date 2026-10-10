# Entrada MCP confiável

Clientes continuam em `127.0.0.1:49374/mcp`. Este processo só encaminha.
`memory_write_page` é a única chamada interceptada: a intenção é gravada
na outbox **antes** de qualquer envio ao backend configurado (nesta máquina,
`127.0.0.1:49377`).

A outbox fica fora do diretório de dados do ai-memory. O padrão é
`~/.local/share/ai-memory-delivery` (intenções e recibos, não a base).
A comparação de caminho usa `relative_to`, para `ai-memory-delivery` não
ser tratada como filha de `ai-memory`.

## O que o recibo diz

- `PENDING`: a escrita está durável e ainda não foi confirmada. `page_id`
  é nulo. Não há `checkpoint`.
- `DELIVERED`: o backend gravou e a leitura de retorno conferiu o corpo e
  os metadados (tier, pinned, tags, title, expires_at).
- `CONFLICT` / `REFUSED`: nada foi sobrescrito. O conflito permanece na
  fila até `reconcile --accept-current-head`.

Escopo ausente, parcial ou diferente de `workspace`+`project` juntos ou
`scope: "global"` (exato, já sem espaços) é recusado com JSON-RPC
`-32602`. Não há replay num projeto inferido. Leitura global usa
`workspace=default` e `project=_global`, porque `memory_read_page` não
tem campo `scope`.

Jev não se aplica: a decisão é igualdade de código tipado, bytes do corpo,
metadados e UUID. Ausência de página é `error.data.code == "page_not_found"`;
ausência de escopo é `error.data.code == "scope_not_found"`. Uma mensagem que
só contém «not found» não é ausência. Não há chamada de modelo.

## Comandos

```text
reliable-mcp serve --listen 127.0.0.1:49374 --backend http://127.0.0.1:49377 --config CONFIG
reliable-mcp worker --backend http://127.0.0.1:49377
reliable-mcp ready --backend http://127.0.0.1:49377 --workspace NOME --project NOME
reliable-mcp status --outbox CAMINHO
reliable-mcp replay --operation ID --backend URL --outbox CAMINHO
reliable-mcp reconcile --operation ID --accept-current-head --backend URL --outbox CAMINHO
reliable-mcp bootstrap --backend URL --outbox CAMINHO
reliable-mcp install --dry-run|--plan|--stage DIR --unit ARQUIVO
```

`serve`, `worker`, `replay` e `reconcile` recusam euid 0. `install` não
instala: gera plano ou um stage. A troca de units fica com o root, depois
da revisão. Ver `PROCEDURE.md`.

`ready` fala com o backend (initialize, tools/list com `create_only` e
`expected_page_id`, e `memory_status` no escopo explícito). HTTP 200 com
erro JSON-RPC, `isError`, 405, 500 ou corpo malformado não contam.
`/healthz` isolado não conta. O cache de schema não responde prontidão.
O instalador também exige ingress e delivery ativos e repete a leitura
MCP pela entrada; backend pronto sozinho não conclui a instalação.
O patch do instalador prioriza `AI_MEMORY_BACKEND_URL`; sem override,
transforma `service.backend_bind` do manifest em origin HTTP, validada por
`ready`. Só a ausência desse campo permite o fallback legado `:49376`.
O stage ainda gera `:49376`; nesta máquina essa porta pertence a
`farol-firecrawl-mcp.service`. Ajuste o stage para `:49377` conforme
`PROCEDURE.md` antes da instalação.

## Limites

- Não é uptime de 100%. Se o relay morre depois de receber o pedido e
  antes do commit atômico (tmp, rename, fsync do arquivo e do diretório),
  o cliente vê erro e a escrita não está durável.
- Se morre depois do commit e antes da resposta, a operação continua
  `pending` e o worker a retoma.
- Conflito não é reescrito sozinho. Um update offline não cobre um head
  mais novo de outra sessão.
- Sem baseline conhecido, a recuperação só cria com `create_only` se o
  caminho estiver ausente. Conteúdo divergente fica `conflict`.
- O backend precisa anunciar `create_only` e `expected_page_id`. Campos
  CAS não são enviados a uma versão que os ignoraria.
- O backend desta variante anuncia os campos CAS. Versões anteriores sem
  esse contrato reprovam a prontidão; o cutover segue `PROCEDURE.md`.
- Lote JSON-RPC é encaminhado opaco e não é interceptado.
- `Content-Length` é obrigatório quando há corpo. Corpo, concorrência e tempo têm teto
  (10 MiB, 32 em voo, encaminhamento ~8 s).
- O cache guarda só schema, `serverInfo`, protocolo e capabilities. Não
  guarda resultado de ferramenta nem segredo.
- Token de autorização não entra em recibo nem em log. Autorização e
  identidade ficam em sidecars privados `0600` para preservar o ator no
  replay. Se o segredo não puder ser relido, a operação fica pendente e
  exige reautorização. Root não faz replay de escrita de outro usuário.
- Redirect para fora do loopback vira 502 e não é seguido.
- O worker espera 1 s entre passagens. É um laço local, não um timer de modelo.
