# CLASSIFICACAO-3.4.2.md

A atualização direta, preservando o comportamento do fork, está **REPROVADA**. Os **82 commits foram reconciliados**: 12 equivalentes, 35 com comportamento ainda ausente, 2 obsoletos, 29 fora do binário e 4 dúvidas explícitas.

Conforme a última instrução, este documento foi entregue somente em stdout. Nenhum arquivo foi criado, nenhum commit foi feito e a worktree permaneceu limpa.

Referências examinadas:

- Fork: `a4d9394f`.
- Destino: `v3.4.2`, commit `b9453c96`.
- Universo: os 82 `+` de `git cherry v3.4.2 a4d9394f`.
- Evidências da tag foram lidas com `git show v3.4.2:<caminho>`. Patch-id serviu apenas para enumerar os commits.

Nas referências abreviadas abaixo, `CLI`, `CORE`, `STORE`, `WIKI`, `HOOKS`, `LLM`, `MCP`, `CONS`, `WEB` e `WORKSTREAM` significam, respectivamente, `crates/ai-memory-{cli,core,store,wiki,hooks,llm,mcp,consolidate,web,workstream}/src`. As referências dessas abreviações apontam sempre à **v3.4.2**. Arquivos novos são destinos propostos.

As estimativas de porte são de linhas de produção, sem testes e documentação. Não são medições nem devem ser somadas: alguns portes compartilham alterações.

## Classificação dos 82 commits

`portar` identifica comportamento ausente e aplicável; a prioridade aparece no encerramento. Em commits parcialmente absorvidos, a estimativa cobre somente o restante.

| Commits locais | Classe | Prova ou menor porte |
|---|---|---|
| `c4ea6bb3` | portar | Excluir dos matches a entidade igual ao nome normalizado do projeto, preservando as demais. Destino `STORE/reader.rs:7797`; **20–40 linhas**. O novo observation stream não elimina esse voto. |
| `bd524699`, `585ab238`, `60593c0b`, `36cba6dc` | portar | Dream local: plano/apply, escopos exatos, modelos locais, gate semântico, preservação literal e de TTL, atividade/deadline e CAS. Destinos `CLI/{config.rs,cli.rs,commands/serve.rs,commands/dream.rs}`, `CONS/{dream.rs,dream_semantics.rs,dream_activity.rs}`, `LLM/dream_local.rs`, `MCP/{dream.rs,admin.rs}`, `STORE/dream.rs`, `WIKI/wiki.rs`; **2.500–4.000 linhas**. Pode ser adiado com o agendamento desligado; a capacidade manual também seria perdida. |
| `3a4a8d79`, `5cc3d4eb` | portar | Relações exatas existentes, tratamento de sentinelas sem apagar páginas legítimas, curadoria de metadados com head+hash e reconciliação de sessões/diagnósticos. Destinos `WIKI/{markdown.rs,wiki.rs,wiki/curation.rs}`, `STORE/{ops.rs,session_consolidation.rs}`, `CONS/{consolidator.rs,types.rs}`, `CORE/page.rs`, `MCP/{admin.rs,server.rs}`, `CLI/commands/serve.rs`; **450–750 linhas**. |
| `d6db29ad` | portar | Reautorizar o escopo efetivamente escrito dentro da transação, também no retry e no vínculo de managed run; revalidar cache web. Destinos `HOOKS/router.rs`, `STORE/{ops.rs,writer.rs,workstream.rs,error.rs}`, `WEB/routes/api.rs`; **140–240 linhas**. A checagem upstream anterior ao loop não equivale à autorização transacional local. |
| `4cfbdf60` | portar | Endpoint separado para queries, conservando a identidade dos embeddings de documentos. Destinos `CLI/{config.rs,commands/serve.rs}`, `LLM/{factory.rs,embedding.rs}` e template de config; **70–120 linhas**. |
| `80dd19cd` | portar | Importer sem redirects e XML/plist já estão cobertos. Resta o deadline abrangendo pipes herdados, com terminate/kill/reap no macOS. Destino `companions/ai-memory-macos/Sources/AIMemoryMenuCore/ProcessRunner.swift`; **50–85 linhas**. Opcional para o servidor Linux. |
| `bb48faa8`, `d7d01799`, `c4d99deb`, `18903744`, `4a9564e5` | fora-do-binario | Ingress Python durável e validação dos envelopes/argumentos. Continuam úteis, após adaptar os contratos de leitura e ausência descritos abaixo. |
| `143ad28a` | portar | O CAS básico existe. Faltam lock entre processos/instâncias de Wiki, recusa de arquivo divergente ou não indexado e vínculo entre `page_id` e corpo retornado. Destinos `WIKI/wiki.rs`, `STORE/{reader.rs,writer.rs}`, `MCP/server.rs`; **180–300 linhas**, reutilizando `PagePrecondition`. |
| `b753a938` | fora-do-binario | Documentação da escrita condicional. Útil depois de atualizar as diferenças entre o contrato local e o da tag. |
| `d0eb4754` | portar | Ausência tipada: `error.data.code=page_not_found/scope_not_found`, sem tratar autorização ou IO como ausência. Destino `MCP/server.rs:2248` e `:6170`; **12–25 linhas**. |
| `30ce45eb` | fora-do-binario | Corrige contexto do patch Python/backend. Útil somente após adaptar o patch à tag. |
| `67e370b8` | fora-do-binario | Testes sintéticos de título/readback e seleção explícita de binário legado. Úteis para qualificar o relay adaptado. |
| `26c60382`, `613d5b02`, `6fe382f3` | fora-do-binario | Fixture e patch do instalador: readiness, bind e escopo do manifest. Úteis, mas a fixture histórica não representa o instalador da 3.4.2. |
| `8c107fa4`, `d25ef35f`, `63cfcb2d` | fora-do-binario | Documentação de bootstrap, credenciais, readiness e guards do ingress. Útil após revisão dos contratos; changelog local permanece histórico. |
| `c34c2c99`, `3b38f76b` | fora-do-binario | fsync do outbox e recuperação de atualização offline com expected head. Continuam úteis, dependendo de CAS/readback correto. |
| `a9735837` | fora-do-binario | Teste de cmdline vazia durante a partida do binário legado. Útil em ensaio sintético, sem selecionar binário instalado implicitamente. |
| `f3f00446` | portar | Embedding integral por chunks UTF-8 e pooling ponderado pelos bytes da fonte no caminho compat local restrito. Destinos `LLM/{embedding.rs,dream_local.rs}`, `CLI/commands/serve.rs`; **130–220 linhas**. A tag trunca a entrada em 8.000 bytes. |
| `00a7b77a`, `be5b467b` | fora-do-binario | Worker de hook delivery: arquivamento somente após `completed_at` e manutenção do mesmo FD do drain lock. Continua útil; exige corrigir recibos de SessionEnd e impedir drain TS concorrente. |
| `eb1ba452` | portar | Claim da nova chave de um SessionEnd já encerrado, reparo dos efeitos e conclusão do recibo. Destinos `STORE/ops.rs:3529`, `HOOKS/router.rs:5123`; **15–30 linhas**, preservando os novos campos de recuperação upstream. |
| `1f0ff776`, `ad3d8c73`, `d2a57a10` | duvida | Alterações amplas de ranking: janelas FTS, snapshot, midranks, força de votos/seeds, união de scopes e refatoração RRF/sanitização. A implementação upstream mudou e acrescentou observation stream. Faltam fixtures sintéticas de relevância, empates e concorrência, com o stream ligado e desligado. |
| `0511bba4` | portar | Cache limitado de vetores de query por instância, com TTL, sem cache de resultados/documentos. Destinos `LLM/{query_embedding_cache.rs,embedding.rs,lib.rs}`; **100–150 linhas**. A tag solicita inferência a cada chamada. |
| `0379a6fa`, `9cfa8dc3` | portar | Handoff automático somente quando existir pendência tipada, preservando turn checkpoints. Destinos `HOOKS/{router.rs,synth.rs}`; **70–120 linhas**. Desligar todos os handoffs não reproduz a seleção local. |
| `c2b0a77d` | fora-do-binario | Teste adversarial de binding legado/proveniência moderna. Útil após adaptar as APIs de escrita. |
| `eebdd33c` | portar | Scrub antes do cap no backstop de assistant. A tag cobre vários outros caminhos, mas ainda corta `protocol.excerpt` antes do sanitizer. Destinos `HOOKS/{assistant_capture.rs,router.rs}`; **15–35 linhas**, usando o sanitizer configurado. |
| `c0747f4c` | igual | Restore recusa snapshot ausente/vazio; a tag também exige histórico de migrações. Prova: `v3.4.2:crates/ai-memory-store/src/lib.rs:185` e `crates/ai-memory-cli/src/commands/restore.rs:247`. Upstream `8b1f52d8`. |
| `d6a28c77` | portar | Download atômico principal já existe em `CLI/http_client.rs:600`. Restam scrub antes do cap no extrator de transcript e preservação de symlink/recusa de hardlinks no destino. Destinos `WORKSTREAM/transcript.rs:3751`, `CLI/http_client.rs:656`; **45–90 linhas**. |
| `d625cb55`, `49ed49e5` | fora-do-binario | Procedimento de release/cargo-deny e instruções para registrar o manifest local. Úteis para releases do fork. `49ed49e5` imprime instruções; não atualiza sozinho o manifest. |
| `ff4682e6` | fora-do-binario | Supressões de lint em traits async, sem mudança de execução. Úteis somente conforme a ferramenta de lint utilizada. |
| `a032e4c6` | duvida | Supressões de `result_large_err` e iteração de helpers SHA locais. Falta executar clippy depois de selecionar os portes: parte do alvo virou `ApiFailure`; outra parte depende de curadoria/dream. |
| `0b9f2599` | fora-do-binario | Sintaxe de helper de teste. Útil somente se o teste correspondente for levado. |
| `c109595c`, `79c0ed30` | obsoleto | Version bumps `2.5.3-local.1/.2`, substituídos pela versão de destino. Prova: `v3.4.2:Cargo.toml:38`, versão `3.4.2`. |
| `1dbb5a16`, `698a11ab`, `d03f65ec`, `fca5fc6c`, `f188fd23` | igual | Correções de dependências presentes ou superadas: rustls `0.23.46`, quinn-proto `0.11.19`, webpki `0.103.16`, libz-sys `1.1.31`, tokio `1.54.1` e edges Windows corrigidas. Provas: `v3.4.2:Cargo.lock:3510`, `:3002`, `:3574`, `:2448`, `:4232`; `Cargo.toml:65` desliga o recurso Askama que trazia basic-toml. Upstream: `bc851df6`, `61db65ce`, `47a2a49d`, `19856fb9`, `b69cf681`, `54c035f3`. Detalhes de zip abaixo. |
| `29549016` | portar | Timeout configurável somente para documentos compat, incluindo o corpo da resposta. Destinos `CLI/config.rs`, `crates/ai-memory-cli/templates/config.default.toml`, `LLM/{embedding.rs,factory.rs}`; **55–95 linhas**. Default local: 120 segundos; faixa 1–1800. |
| `1d93add8`, `62baa09e` | portar | Expor `from_session_id` na listagem administrativa de handoffs e degradar valor inválido para `null`, preservando o item. Destinos `STORE/reader.rs:8189`, `:16079`, `MCP/admin.rs`; **15–30 linhas**. |
| `e4624c25` | fora-do-binario | Changelog da extensão local `from_session_id`. Útil somente se a extensão for preservada. |
| `59873022` | portar | Pular handoffs indecodificáveis sem esconder os demais, sem promover automático para manual e sem engolir falhas SQLite. Destino `STORE/reader.rs`, nos leitores de handoff; **40–70 linhas**. |
| `985e96d2` | portar | TTL de recent já foi corrigido; resta aplicar `LIMIT` no SQL do caminho sem cursor. Destinos `STORE/reader.rs`, `WEB/routes/api.rs:640`; **30–55 linhas**, preservando a paginação nova. |
| `f3fb8aac` | portar | A tag repara nomes DOS e trailing dot, mas não toda a reparação local de segmentos `.`, vazios, leading slash e nomes reservados do Git. Destino `CONS/path_sanitize.rs:43`; **35–65 linhas adicionais**. |
| `b078dcb1` | portar | Filtros `title`/`body` pontuados em minúsculas já existem. Resta reconhecer os prefixos ASCII sem distinguir maiúsculas. Destino `STORE/fts_query.rs:401`, `:448`; **10–25 linhas**, conservando parênteses e prefixo `*` upstream. |
| `9af82b0d` | igual | Entrada FTS limitada antes do parsing a 4.096 bytes, respeitando UTF-8; a tag também trata NUL. Prova: `v3.4.2:crates/ai-memory-store/src/fts_query.rs:348`. Upstream `b72d0df3`. |
| `2c24edda` | fora-do-binario | Corrige somente um assert para procurar `--agent grok`, em vez de uma substring possivelmente presente no caminho do executável. Não altera o parser. |
| `c3b5a60e` | portar | `tool_label` validado e limitado, inclusive em metadata-only. Destinos `HOOKS/{capture_policy.rs,payload.rs,router.rs}`; **45–80 linhas**. O `record_tool_names` upstream tem outro contrato. |
| `2d26dae5` | portar | Descoberta de `hooks/` por walk-up de exe/cwd/manifest, incluindo target externo e triple. Destinos `CLI/{install_layout.rs,commands/install_hooks.rs,commands/setup_agent.rs}`; **60–95 linhas**. Este commit é de descoberta, não de tool-label. |
| `d009b152` | portar | Impedir que o drain TS sem lock/recibo remova envelopes pertencentes ao sidecar. Destino `CLI/commands/render_shared.rs:3167`; **1–5 linhas** para desabilitar a chamada, seguidas da regeneração das integrações. Implementar recibos seria um porte maior. |
| `59a33000`, `1542b29f` | fora-do-binario | Comentários/docs sobre crescimento do spool TS com drain desabilitado e descarte do backlog nativo após sete dias. Úteis; o primeiro texto pressupõe `d009b152`. |
| `21b7879c`, `d0d13651` | portar | A recusa de redirects nos caminhos normais já existe. Restam fallbacks que voltam a `Client::new()` com policy padrão. Destinos `CLI/http_client.rs:337`, `CLI/commands/hook_capture.rs:933`; **5–15 linhas**. |
| `1f6a55f7` | igual | Guard de lápide na transação, substituição manual legítima e preservação de `purged_sessions` no sweep. Provas: `v3.4.2:crates/ai-memory-store/src/ops.rs:211`, `:1121`, e `crates/ai-memory-wiki/src/watcher.rs:810`. Upstream `71ef6118`, `be03d595`. |
| `a49e2a76` | igual | Markup do harness excluído dos prompts de handoff e da pergunta candidata. Provas: `v3.4.2:crates/ai-memory-hooks/src/synth.rs:371`, `:377`, e `crates/ai-memory-hooks/src/router.rs:5955`. Upstream `f7d71091`. |
| `ebb36881` | fora-do-binario | Amplia testes de lápide, versão viva e FTS. Continua útil como cobertura; o guard de runtime já existe. |
| `93d3e145` | igual | Saída de grants e mensagens neutraliza controles; a tag também protege as cercas de texto não confiável. Provas: `v3.4.2:crates/ai-memory-cli/src/commands/grant.rs:98` e `crates/ai-memory-cli/src/commands/message.rs:436`. Upstream `4e32ec54`. |
| `404fa8a0` | portar | Política local de recusa total de redirects. A tag bloqueia mudança de origem, mas segue redirects na mesma origem. Destinos `LLM/redirect.rs` e construtores, `WIKI/admission.rs`; **15–30 linhas** para manter a política local. |
| `7a65a994` | igual | Contexto somente de respostas 2xx no shell, PowerShell, nativo e geradores TS. Provas: `v3.4.2:hooks/_lib.sh:1106`, `hooks/lib/ai-memory-hook.ps1:1184`, `crates/ai-memory-cli/src/commands/hook_capture.rs:1215`. Upstream `5a758934` (#1986), `15e530fd`, `7779d8a9`. |
| `8f272d97` | portar | Grande parte entrou em #1987. Restam nomes de workstream/lease em `run` e tags em `upgrade` sem filtro. Destinos `CLI/commands/{run.rs,upgrade.rs}`; **12–25 linhas**. |
| `b1389f55` | portar | Hook warning, preview e rename estão cobertos. Resta a garantia de linha única em `finalize_session`: a tag usa `terminal_body`, que preserva newline/tab. Destino `CLI/commands/finalize_session.rs:308`; **1–3 linhas**. |
| `56758c2c` | portar | Nome do agente ainda bruto na sugestão `install-hooks` do doctor, embora saneado na tabela. Destino `CLI/commands/doctor/presentation.rs:145`; **1–3 linhas**. |
| `a4d9394f` | igual | Restore de checkpoint/versão usa `upsert_page_from_disk`, desfaz a escrita do arquivo em erro e responde HTTP 409 para `SessionPurged`. Provas: `v3.4.2:crates/ai-memory-wiki/src/wiki.rs:1472`, `:1491`, e `crates/ai-memory-mcp/src/admin.rs:4816`. Upstream `d330c59f` (#1989). |

A referência `93d3e136` do enunciado foi reconciliada com o commit efetivamente enumerado, `93d3e145`, relativo a grants/mensagens.

## 1. Embeddings e aceitação da configuração

**A v3.4.2 não oferece separação nativa dos endpoints de query e documento equivalente a `embedding_query_base_url`. Essa chave no nível raiz é ignorada; sua presença não impede a partida.**

A prova combina desserialização e construção do embedder:

- `CLI/config.rs:283` explica expressamente que o `Config` raiz **não** usa `deny_unknown_fields`; sua anotação é `#[serde(default)]`.
- `CLI/config.rs:2466` incorpora o TOML, e `:2485` executa `figment.extract()`.
- `TaskLlmSettings`, em `CLI/config.rs:267`, usa `deny_unknown_fields`, mas essa restrição pertence àquela struct. Não torna o `Config` raiz estrito.
- `LLM/factory.rs:204` define um `EmbedderConfig` com um único `base_url`.
- `LLM/embedding.rs:476` mantém um único cliente e um único `base_url`; `embed_document`, em `:579`, e `embed_query`, em `:592`, usam ambos.

Os prefixos de query/documento distinguem o texto enviado, não o endpoint. O wrapper de capacidade, em `LLM/embedding_capacity.rs:55` e `:139`, separa capacidade de execução, mas delega ao mesmo embedder. A seleção de provedores por workload de LLM também não substitui esse roteamento de embeddings.

Consequência: sem `4cfbdf60`, queries passam a usar `embedding_base_url`, silenciosamente. Um proxy externo poderia acrescentar roteamento próprio, mas isso não é uma alternativa implementada pela tag.

| Comportamento local | Na v3.4.2 |
|---|---|
| `embedding_query_base_url` | Chave ignorada; query e documento usam o mesmo endpoint. |
| `embedding_document_timeout_secs`, de `29549016` | Chave também ausente e ignorada no nível raiz. O cliente compat tem timeout fixo de **120 segundos por requisição**, em `LLM/embedding.rs:515`, incluindo o consumo da resposta. Não existe orçamento configurável separado para documentos. |
| Embedding integral, de `f3f00446` | Ausente. `LLM/embedding.rs:349` chama `truncate_for_embedding`; `LLM/text.rs:36` impõe o teto de **8.000 bytes**. O prefixo entra antes do corte. |
| Cache de query, de `0511bba4` | Ausente. A chamada chega novamente a `embed_query`. |

O embedding integral do fork era específico do cliente compat local restrito: chunks completos, limites de JSON e pooling ponderado. Não era uma mudança universal de todos os provedores.

Há uma ligação relevante com dream: no fork, `CLI/commands/serve.rs:2474` seleciona `local_only` quando `dream.scopes` não está vazio, independentemente de `dream.enabled`. Portanto, **`dream.enabled=false` sozinho não prova que o embedding integral estava inativo**.

Ao abandonar esse modo, documentos novos passam a ter outra cobertura textual. O mesmo `{provider, model, dim}` não identifica essa diferença de algoritmo; a atualização não implica reembedding automático seguro do acervo existente.

## 2. Ranking e fixbrainrank

**`c4ea6bb3` continua aplicável.**

Em `STORE/reader.rs:7797`, `entity_hits_filtered` monta matches exatos e por prefixo. O SQL de `:7866` restringe workspace/projeto, mas não exclui a entidade cujo nome seja o próprio projeto. O peso inverso de frequência em `:7881` reduz entidades comuns; não implementa a exclusão local.

O caminho híbrido continua consumindo o stream de entidades em `STORE/reader.rs:8699` e seus votos RRF em `:8939`.

O novo `[retrieval] observation_stream`:

- Tem default `true`: `STORE/retrieval_tuning.rs:126`, propagado por `CLI/config.rs:2125` e `:2146`.
- Acrescenta votos de observações correspondentes, uma vez por sessão, para a página de sessão mais recente: `STORE/reader.rs:8824`.
- Não substitui os votos de entidades nem elimina os seeds que eles já produziram.

Assim, o menor porte é a exclusão exata e normalizada do nome do projeto no stream de entidades. Não há justificativa para trazer junto toda a reformulação local de ranking.

Os três commits amplos ficaram em `duvida` porque equivalência ou ganho de relevância exigem fixtures com os novos streams, empates e concorrência. Não foi medido resultado de busca sobre dados reais.

## 3. Relay: rotas, métodos, cabeçalhos e respostas

O relay é **ingress → backend**. Os contratos ativos estão em `local-reliable-mcp/reliable_mcp/{backend.py,relay.py,ready.py,protocol.py,hook_delivery.py}`.

### Contratos usados

| Rota/método | Requisição e resposta utilizadas | Situação na v3.4.2 |
|---|---|---|
| `POST /mcp` — `initialize` | JSON-RPC 2.0, ID correspondente, `protocolVersion` e `serverInfo`. Readiness oferece `2024-11-05`. | Rota presente em `CLI/commands/serve.rs:1847`. O código mantém handshake legado e respostas JSON no modo padrão, em `:1604`. A negociação não foi ensaiada contra um processo da tag. |
| `POST /mcp` — `tools/list` | `result.tools`; verifica tipos de `expected_page_id` e `create_only`, não apenas seus nomes. | Campos presentes em `MCP/server.rs:2119` e `:2124`. |
| `POST /mcp` — `memory_status` | `result.content[0].text` contendo JSON; exige `counts.pages_latest` inteiro e `scope.{workspace,project,resolved_by="explicit"}`. | Formato presente em `MCP/server.rs:7684`; embalagem textual JSON em `:8959`. |
| `POST /mcp` — `memory_read_page` | Página com corpo/frontmatter/título e `page_id` válido; o relay compara o corpo integral. Ausência deve ser tipada. | **Contrato incompleto**: ausência sem os códigos locais, corpo truncável e ID obtido separadamente do corpo. |
| `POST /mcp` — `memory_write_page` | Argumentos de página mais `expected_page_id` ou `create_only`; sucesso com `page_id`/checkpoint. Após erro ou resposta ambígua, faz readback. | CAS e formato básico presentes, em `MCP/server.rs:5428`, `:8880` e `STORE/ops.rs:307`. Faltam garantias locais de arquivo/readback. |
| `POST /hook?...` | JSON do evento; query com `event`, `agent`, workspace/projeto e `ingest_key`, além dos campos de sessão aplicáveis. Worker usa requisição individual para eventos lentos, como SessionEnd/PreCompact. | Presente em `HOOKS/router.rs:743`; a resposta de ingresso é HTTP **202**, corpo `queued`, em `:920`. Saturação pode produzir 429. Aceitação não prova processamento concluído. |
| `POST /hook/batch` | Array JSON de `{url, body}`. | Mesmo formato em `HOOKS/router.rs:923`. Resposta `HookBatchAck`, em `:1005`, contém `accepted`, `results` e campos adicionais compatíveis. Não substitui o recibo SQL exigido pelo worker local. |
| `GET /healthz` | Probe de testes/partida, JSON `{"status":"ok"}`. | Presente em `CLI/commands/serve.rs:3380`. Não é readiness de CAS ou de armazenamento. |
| Encaminhamento genérico | `GET`, `POST`, `PUT`, `DELETE`, `OPTIONS`, `HEAD`; somente `POST /mcp` recebe tratamento especial. Preserva alvo/query e repassa status, cabeçalhos e corpo. | O relay não contém uma lista finita de endpoints adicionais. `/handoff`, `/brief`, `/recap` continuam GET em `HOOKS/router.rs:755`; outros alvos dependem das rotas do servidor. O encaminhamento não promete que todo método exista em todo alvo. |

`/web`, `/api/v1/*` e `/admin/*` podem atravessar o encaminhamento genérico; não são APIs especiais implementadas pelo relay. Não há handler `PATCH` local.

### Cabeçalhos

`backend.py:69` constrói chamadas MCP com:

- `Host`;
- `Content-Type: application/json`;
- `Accept: application/json, text/event-stream`;
- `Content-Length`;
- `Authorization`, quando fornecido;
- cabeçalhos adicionais de identidade preservados da requisição original.

`relay.py:54` e `:392` preservam `Origin`, `Cookie`, `Mcp-Session-Id`, `Mcp-Protocol-Version`, `Host` e `X-Memory-*`. `backend.py:156` remove cabeçalhos hop-by-hop e mantém os cabeçalhos de ponta a ponta. O Host original do ingress pode ser preservado; `allowed_hosts` do backend precisa admiti-lo.

O cliente usa `HTTPConnection`; não segue redirects automaticamente. Apesar do `Accept` anunciar SSE, seu parser processa JSON, não um stream SSE. Deve-se manter o modo HTTP JSON padrão da tag, sem depender de `--http-stateful`.

O worker de hooks envia `Content-Type: application/json` e, quando configurado, `Authorization: Bearer …`.

### Escrita condicional: o que existe e o que falta

A tag aceita:

```json
{"create_only": true}
```

ou:

```json
{"expected_page_id": "<UUID>"}
```

O parser cria `PagePrecondition`, e o writer testa o head na transação. Portanto, **não é necessário portar novamente o CAS básico**.

Restam três diferenças relevantes:

1. **Ausência tipada.**
   `MCP/server.rs:2248` e `:6170` retornam `invalid_params` sem `data.code`. O relay, em `backend.py:139`, reconhece ausência somente por `page_not_found` ou `scope_not_found`. Sem adaptação, uma criação legítima pode ficar rejeitada/pendente.

2. **ID ligado ao corpo.**
   Em `MCP/server.rs:6079`, o head é consultado antes da leitura do arquivo em `:6098`. A resposta pode combinar um ID com conteúdo de outro momento. O fallback SQL também é uma consulta separada.
   `WIKI/wiki.rs:3669` valida o head SQL, mas não reproduz todos os guards locais de arquivo divergente/não indexado e lock entre processos.

3. **Leitura integral.**
   `MCP/server.rs:1883` define default de 12.000 caracteres e máximo de 64.000. O relay não envia `max_chars` e exige igualdade do corpo completo. A resposta informa `truncated`, mas o contrato local não resolve isso.
   É necessário adaptar leitura/verificação para o tamanho de página admitido pelo relay; simplesmente pedir 64.000 não resolve páginas maiores. Este é um ajuste de compatibilidade adicional da subida.

O código numérico do conflito também mudou: a tag usa erro de parâmetros com `reason=precondition_failed`, enquanto o local tinha erro/código próprios. O relay trata erro RPC por readback; não exige transportar o número antigo.

### Recibos do worker

`hook_delivery.py:95` abre SQLite somente para leitura. Em `:160`, consulta:

```sql
SELECT seen_at, completed_at
FROM ingest_keys
WHERE project_id = ? AND key = ?
```

Só arquiva o envelope após `completed_at` preenchido. As colunas continuam disponíveis na tag, inclusive após V77.

O caso restante é `eb1ba452`: em `STORE/ops.rs:3529`, um SessionEnd já encerrado pode retornar antes de criar claim para uma nova chave. `HOOKS/router.rs:5123` completa somente uma chave já claimed. Para o worker local, essa entrega pode nunca adquirir recibo concluído.

### Testes executados

Comando executado dentro de `local-reliable-mcp`:

```bash
python3 -B -m unittest -v \
  tests.test_title_contract.TitleContractCase \
  tests.test_title_contract.LegacyBinarySelectionCase.test_missing_legacy_override_does_not_select_installed_binary \
  tests.test_rpc_contract.CasSchema \
  tests.test_rpc_contract.ParseMessage
```

Resultado:

```text
Ran 9 tests
OK
```

Foram cinco testes de título/readback, um de seleção de binário legado ausente, um de schema CAS e dois de parsing JSON.

Também foram tentados estes testes de `ReadinessEnvelope`:

- `test_wrong_id_or_version_is_not_ready`;
- `test_error_is_error_and_bad_scope_stay_failed`;
- `test_typed_backend_schema_is_ready_and_presence_only_is_not`.

Ficaram bloqueados por `PermissionError: [Errno 1] Operation not permitted` na criação de sockets, antes de iniciar o backend sintético.

A suíte completa, os testes de persistência/outbox e uma integração com o binário 3.4.2 não foram executados. Não foi iniciado servidor nem consultado banco real.

## 4. Hooks

| Tema | Constatação e prova |
|---|---|
| Escopos — `d6db29ad` | `HOOKS/router.rs:4911` verifica grants antes do loop. Admissão/retry em `:5022` e `:5045` não reproduzem a autorização transacional local sobre o escopo efetivo. `STORE/ops.rs:3390` verifica identidade/tupla, mas não recebe a autorização necessária para repetir o guard local de grants. O porte deve cobrir retry, mudança de escopo e vínculo de managed run, com teste adversarial e controle legítimo. |
| Cache incluído no mesmo commit | `WEB/routes/api.rs:99` ainda permite cache privado com `max-age`. `with_no_store`, em `:113`, existe para alguns caminhos; não equivale à revalidação local de todas as respostas afetadas. |
| Tool-label — `c3b5a60e` | `HOOKS/payload.rs:1099` e a allowlist em `HOOKS/router.rs:1420` não preservam o campo local validado. `record_tool_names` é uma opção de título; metadata-only remove o nome em `:1443`. |
| Descoberta — `2d26dae5` | `CLI/commands/install_hooks.rs:6977` e `commands/setup_agent.rs:970` usam candidatos limitados de localização. Falta o walk-up local para outros layouts de target. |
| `from_session_id` | O domínio possui a origem da sessão, mas `STORE/reader.rs:8189` não a seleciona para `OpenHandoff`; o tipo em `:16079` não a expõe. Os dois commits de runtime continuam necessários para essa resposta administrativa. |
| Handoff por markup — `a49e2a76` | Equivalente: `HOOKS/synth.rs:371`/`:377`, e `router.rs:5955`. |
| Handoff somente com pendência — `9cfa8dc3`, `0379a6fa` | Ausente. A condição em `HOOKS/router.rs:5530` controla se cria handoff, mas o builder em `:5840` retorna um handoff, não uma decisão opcional baseada na pendência tipada. |
| Contexto só em 2xx — `7a65a994` | Equivalente nos caminhos examinados; referências na classificação. |
| Drain TS — `d009b152` | `CLI/commands/render_shared.rs:3167` ainda gera drain ativo, sem compartilhar o lock/recibo SQL do worker local. Pode remover o arquivo ao receber sucesso HTTP ou rejeição permanente. |
| Spool — `59a33000`, `1542b29f` | São documentação/comentários. O spool nativo mantém limite de idade de sete dias em `CLI/commands/hook_spool.rs:53` e descarte em `:2280`. Desabilitar o drain TS sem worker funcional deixa crescimento do spool; isso precisa permanecer documentado. |

O guard de escopo deve preservar a colaboração legítima entre sessões e operadores. Aplicar um filtro de proprietário às páginas não é substituto para autorizar cada escrita.

## 5. Segurança e os três PRs

### Lápide, restauração e reindex

Os guards principais estão presentes:

- `STORE/ops.rs:211` executa `check_page_restore_allowed` antes do upsert, na mesma transação.
- O guard em `:226` verifica a sessão purgada e admite substituição manual legítima conforme a regra existente.
- O sweep em `STORE/ops.rs:1121` preserva as lápides necessárias.
- `WIKI/watcher.rs:810` trata a recusa na reindexação.
- Restore de checkpoint/versão passa por `upsert_page_from_disk`, em `WIKI/wiki.rs:1472`, com rollback do arquivo em `:1491`.
- `MCP/admin.rs:4816` traduz a recusa para HTTP 409.

Assim, `1f6a55f7` e `a4d9394f` estão equivalentes. `ebb36881` acrescenta testes ainda úteis, sem introduzir outro guard de execução.

### Redirects

Há cobertura parcial, não equivalência total com a política local:

- `CLI/http_client.rs:337` e `CLI/commands/hook_capture.rs:933` usam `Policy::none()`, mas voltam a `Client::new()` se o builder falhar. Esse fallback não conserva a política.
- `LLM/redirect.rs` permite redirects na mesma origem e recusa mudança de origem.
- Os construtores de embedding/LLM usam essa política; `WIKI/admission.rs:379` também preserva redirects de mesma origem.

Portanto:

- O bloqueio de redirects entre origens já está coberto.
- A recusa de **qualquer** redirect, de `404fa8a0`, ainda exige porte.
- Os dois fallbacks devem ser corrigidos para conservar a garantia de `21b7879c`/`d0d13651`.

### Texto de terminal

`CLI/show.rs:708` define `terminal_text`, que produz texto de linha única. `terminal_body`, em `:713`, neutraliza controles perigosos, mas conserva newline/tab para corpos multilinha.

Restantes confirmados:

- `CLI/commands/doctor/presentation.rs:145` usa o nome bruto na sugestão de instalação.
- `CLI/commands/run.rs:1120` inclui nome de workstream sem o filtro local; há caminho equivalente para lease.
- `CLI/commands/upgrade.rs:94` e `:192` imprimem tag sem a cobertura local completa.
- `CLI/commands/finalize_session.rs:308` usa a variante multilinha onde o fork passou a exigir linha única.

`CORE/i18n.rs:248` insere valores na formatação sem sanitizá-los. `CLI/terminal.rs:56` imprime o texto; a camada de pintura não acrescenta sanitização. Portanto, esses caminhos não ficam implicitamente protegidos pela tradução ou pelo terminal.

### Scrub antes de truncar

A tag já faz scrub antes do limite em vários caminhos, incluindo ledger e spool. Isso não cobre todos os restantes:

- `HOOKS/assistant_capture.rs:193` corta `protocol.excerpt` antes de aplicar o sanitizer configurado.
- `WORKSTREAM/transcript.rs:3766` limita o conteúdo extraído antes da sanitização posterior.

Esses são os portes remanescentes de `eebdd33c` e de parte de `d6a28c77`.

No restante de `d6a28c77`, o download atômico já existe em `CLI/http_client.rs:600`, com staging, sync e persist. A política de destino difere: em `:658`, symlink não recebe a preservação local do alvo; não há a mesma recusa de hardlink.

### Dependências

As correções dos cinco commits estão presentes ou superadas nas versões citadas na classificação. Para zip, olhar somente o número `8.6.0` seria insuficiente:

- `v3.4.2:Cargo.toml:212` documenta o backport de segurança.
- O patch fixa o commit `507c5c76ddd4cde767e64832fc6f848c51a99e44`.
- Upstream `5c69054d` também adicionou validação de consistência dos headers na extração de release.

Não há motivo para transportar os ignores antigos literalmente. Esta conclusão cobre os problemas desses commits; não equivale a um novo `cargo audit` com base atualizada de advisories.

### PRs do dono

**Os três entraram na ancestralidade da v3.4.2.**

| PR | Commit upstream | Prova |
|---|---|---|
| #1986 | `5a758934` | `git merge-base --is-ancestor 5a758934 v3.4.2` retornou 0. Cobertura de respostas 2xx em hooks. |
| #1987 | `032a187f` | Mesmo teste retornou 0. Cobertura ampla de texto de terminal, com complementos upstream. |
| #1989 | `d330c59f` | Mesmo teste retornou 0. Restore respeitando lápide e rollback. |

A entrada de #1987 não prova que os ajustes locais posteriores de terminal também entraram; os restantes foram identificados acima.

## 6. Dream local e curadoria

**Com `dream.enabled=false`, o agendamento permanece desligado na tag. Isso permite adiar o porte do dream, mas não torna suas funcionalidades equivalentes.**

`CLI/config.rs:2019` aceita somente estes campos conhecidos:

```text
enabled
interval_secs
idle_window_secs
min_pts
max_eps
max_clusters_per_run
min_cold_pages
```

Os defaults em `:2046` são `enabled=false`, intervalo 3.600 segundos e os demais parâmetros numéricos em zero para seleção dos defaults efetivos.

A struct da tag usa `#[serde(default)]`, sem `deny_unknown_fields`. Os campos locais:

```text
dream.scopes
dream.llm_endpoint
dream.llm_model
dream.jev_model
```

**São ignorados**, não rejeitados. O fork os validava estritamente.

O que fica para trás:

- execução manual de planejamento e apply, mesmo com agendamento desligado;
- allowlist de scopes exatos;
- clientes locais dedicados, sem herdar credenciais/fallbacks do LLM principal;
- gate semântico local e preservação literal/negativa;
- recusas locais envolvendo TTL e evidência;
- coordenação local de atividade/deadline;
- parte das garantias adicionais de snapshot/head/hash.

A tag possui seu próprio dream. `CLI/commands/serve.rs:2550` escolhe o provider do workload Dream; o scheduler em `:2725` percorre os scopes existentes. Não usa a allowlist local ignorada.

Também há CAS upstream no dream, em `CONS/dream.rs:810`; seria incorreto afirmar que todo controle de concorrência está ausente. A preservação de keep-tokens em `:666`, porém, não reproduz a recusa literal local: perda pode virar aviso.

A curadoria de `3a4a8d79`/`5cc3d4eb` também não foi substituída integralmente pelo curator genérico:

- `WIKI/markdown.rs:373` normaliza referências e acrescenta `.md` em `:402`; não exige a mesma existência exata do alvo.
- `STORE/ops.rs:2407` insere relações tipadas sem o mesmo filtro local de alvo existente.
- A limpeza local de relações/sentinelas, condicionada a head+hash e preservando corpo, não é o contrato do curator genérico.

Pode-se adiar essas ferramentas, mas não assumir que reabilitar `dream.enabled` na tag restaura o modo local.

## 7. Chaves de configuração solicitadas

As chaves abaixo **existem**. Seus defaults e significados principais foram conservados. Isso não implica equivalência de todos os caminhos internos nem validação da configuração real, que não foi procurada.

| Chave | Default na v3.4.2 e semântica | Prova na tag |
|---|---|---|
| `embedding_provider` | `None`: seleção efetiva de embedder local best-effort. `none/off/disabled` desliga; escolha explícita mantém falha explícita. | `CLI/config.rs:1495`, `:3240` |
| `embedding_model` | `None`: default do provider. Local usa `all-MiniLM-L6-v2`; compat exige modelo explícito. | `CLI/config.rs:1496`, `:3265`; `LLM/embedding.rs:23` |
| `embedding_dim` | `None`: inferida conforme provider/modelo; local 384. Compat exige dimensão explícita; zero é recusado. | `CLI/config.rs:1497`, `:3282`; `LLM/embedding.rs:25` |
| `embedding_base_url` | `None`: endpoint do provider. Compat exige URL explícita. Continua sendo um único endpoint para query/documento. | `CLI/config.rs:1498`; `LLM/factory.rs:252` |
| `allowed_hosts` | `["localhost","127.0.0.1","::1"]`; allowlist do Host HTTP. | `CLI/config.rs:1523`; `CLI/commands/serve.rs:1626` |
| `consolidate_on_session_end` | `false`; consolidação de fim de sessão permanece opt-in. | `CLI/config.rs:1485` |
| `capture_assistant` | `false`; captura de assistant opt-in. | `CLI/config.rs:1486` |
| `decay.hard_delete_after_days` | `180`; carência para remoção permanente de cadeias já evictadas/tombstonadas. Não é prazo geral de vida de todas as páginas. | `CLI/config.rs:101`, `:149`; `STORE/decay.rs:100` |
| `decay.observation_retention_days` | `0`; desliga a poda. Quando habilitada, trata observações de sessões consolidadas. | `CLI/config.rs:106`, `:151` |
| `sanitize.extra_patterns` | Lista vazia; acrescenta regex às regras internas. Regras inválidas falham na compilação da configuração. | `CORE/sanitize.rs:661` |
| `routing.mid_session` | `follow-cwd`; modo sticky disponível. | `CLI/config.rs:1373`; `CORE/active_project.rs:128` |
| `auto_improve.scheduler.enabled` | `true`; habilita o scheduler sujeito aos demais gates, sem substituir a invocação manual. | `CLI/config.rs:1824`, `:1847` |
| `maintenance.lint_interval_secs` | `86400`; zero desliga. Cadência de sucesso persiste entre reinícios, também no fork. | `CLI/config.rs:1949`, `:1986` |
| `maintenance.embedding_backfill_interval_secs` | `0`; backfill periódico desligado por padrão. | `CLI/config.rs:1952`, `:1987` |

As regras internas do sanitizer e os providers suportados evoluíram. A compatibilidade das chaves não significa que toda saída seja idêntica à do fork.

## 8. Partida, backup e migrações V71–V99

### `.serve.lock`

**Sim, o guard permanece.**

`CLI/commands/serve.rs:172` define `ServeLock` mantendo o arquivo aberto durante a vida do processo. `:217` abre `.serve.lock`, e `:251` chama `try_lock_exclusive()` via `fs2`, correspondendo a flock no Linux.

O lock é adquirido em `:1256`, antes do backup pré-migração em `:1269` e de `Store::open` em `:1272`.

Há exceções explícitas, incluindo continuação sem o guard por `--force` quando outro holder existe. Não se deve usar esse escape como procedimento normal da subida.

### `pre-migration-backup.json` e `archive_path`

**O mecanismo permanece, mas não garante um snapshot novo para esta atualização.**

- `WIKI/backup.rs:21` define o receipt.
- `:27` contém `archive_path`.
- `:48` carrega o JSON.
- `:55` verifica a presença do arquivo apontado.
- `WIKI/migrations/m2026_09_okf_conformance.rs:69` executa `snapshot_before_db_migration`.
- Em `:75`, receipt existente com archive presente provoca reutilização.
- Em `:81`, wiki já conforme provoca no-op.
- A criação do backup acontece em `:86`.

Esse gate protege a migração de formato OKF de stores antigos. **Não é um backup obrigatório a cada avanço do schema SQLite.** Assim, sua presença não prova cobertura fresca da passagem deste fork para 3.4.2.

### Peso estático das migrações

Foram lidos os SQL V71–V99. A ordem abaixo é uma avaliação estrutural; não foram medidos tempo, espaço ou duração de indisponibilidade. O volume de 1,5 milhão de observações é o informado no pedido.

| Migração/grupo | Trabalho relevante |
|---|---|
| **V97 — `V97__fts_split_path_separators.sql:44` e `:68`** | Recria `observations_fts` e executa `rebuild`, retokenizando título/corpo das observações existentes. Também reconstrói `pages_fts` em `:36`. É o principal custo SQL diretamente proporcional ao texto de todo o acervo de observações. |
| **V87 — `V87__cjk_search_indexes.sql:11` + backfill de abertura** | O SQL cria o índice auxiliar vazio. O custo relevante continua no Rust: `STORE/lib.rs:285` executa o backfill antes de abrir o writer. `STORE/ops.rs:1685` exige uma linha auxiliar para **cada** origem, inclusive texto somente latino. `:1747` lê título/corpo e insere em lotes. |
| **V99 — `V99__observation_file_path.sql:5` e `:7`** | Acrescenta coluna nullable e cria índice parcial. As linhas antigas leem NULL e não geram entradas históricas no índice. Há avaliação da tabela para construir o índice; não é reconstrução do corpo das observações. |
| **V72, V73, V78, V80, V81, V83, V85, V86, V88, V89** | Reconstroem `sessions` para ampliar o conjunto de agentes e recriam índices. Exemplo: `V78__sessions_droid_agent_kind.sql:27`. O custo depende da quantidade de **sessões**, não de copiar 1,5 milhão de observações em cada migração. |
| **V79 — `V79__drop_legacy_user_token_columns.sql:27` e `:70`** | Reconstrução de usuários e verificações de referências/contagens, envolvendo outras tabelas. Pode ter custo relevante, mas não reescreve todos os corpos de observação. |
| **V77 — `V77__recovery_ingest_keys.sql:1`** | Acrescenta estado de recuperação, restrição e índice em `ingest_keys`. O tamanho dessa tabela precisa ser considerado separadamente. |
| **V95 — `V95__message_editing_state.sql:41`** | Reconstrói mensagens; custo proporcional ao histórico de mensagens, não ao volume de observações. |
| **Demais V71–V99** | DDL, índices e ajustes em tabelas de projetos, perfis, runs, credenciais, mensagens, uso e sinais. Não contêm outro rebuild integral do texto de observações equivalente ao V97. |

O backfill CJK usa cursor por rowid e lotes de 256, com checkpoint entre lotes, em `STORE/ops.rs:1675` e `:1747`. A tag já contém a correção que evita reprovar repetidamente todo o prefixo indexado; não se deve atribuir a ela o comportamento quadrático antigo.

A abertura configura transações `IMMEDIATE` e desliga foreign keys durante as reconstruções, religando-as depois: `STORE/lib.rs:243`. Isso evita interpretar cada reconstrução de `sessions` como cascata destrutiva das observações.

## Reconciliação e verificação

O script foi executado com as linhas da classificação em memória. Expandiu cada abreviação para o commit completo e comparou conjuntos exatos com `git cherry`; não contou ocorrências de hashes nas explicações.

Este núcleo permite repetir a conferência fornecendo a tabela acima pelo stdin:

```python
import collections
import re
import subprocess
import sys

def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()

classes = ("igual", "portar", "obsoleto", "fora-do-binario", "duvida")
rows = []

for line in sys.stdin.read().splitlines():
    cells = [c.strip() for c in line.split("|")[1:-1]]
    if len(cells) >= 3 and cells[1] in classes:
        hashes = re.findall(r"`([0-9a-f]{8,40})`", cells[0])
        assert hashes
        rows.append((hashes, cells[1], cells[2]))

cherry = [line.split() for line in
          git("cherry", "v3.4.2", "a4d9394f").splitlines()]
expected = {h for mark, h in cherry if mark == "+"}

entries = [
    (git("rev-parse", "--verify", h + "^{commit}"), kind)
    for hashes, kind, _ in rows for h in hashes
]
found = {h for h, _ in entries}
duplicates = [h for h, n in collections.Counter(
    h for h, _ in entries).items() if n > 1]
counts = collections.Counter(kind for _, kind in entries)

for kind in classes:
    print(f"{kind}={counts[kind]}")
print(f"soma={sum(counts.values())}")
print(f"faltantes={len(expected - found)} "
      f"extras={len(found - expected)} duplicados={len(duplicates)}")

assert len(expected) == len(entries) == len(found) == 82
assert expected == found and not duplicates
assert sum(mark == "-" for mark, _ in cherry) == 0

for _, kind, detail in rows:
    if kind not in ("igual", "obsoleto"):
        continue
    refs = re.findall(
        r"((?:crates|hooks)/[^\s,:`]+|Cargo\.(?:toml|lock)):(\d+)",
        detail,
    )
    assert refs
    for path, number in refs:
        source = git("show", "v3.4.2:" + path).splitlines()
        assert 1 <= int(number) <= len(source)

print("RECONCILIACAO=OK")
```

Saída da conferência executada:

```text
HEAD=a4d9394f
v3.4.2=b9453c96
git cherry: +=82 -=0
igual=12
portar=35
obsoleto=2
fora-do-binario=29
duvida=4
soma=82
classificados=82 unicos=82
faltantes=0 extras=0 duplicados=0
linhas igual/obsoleto com arquivo:linha validado na tag=9/9
PR#1986 commit=5a758934 ancestral_da_tag=sim
PR#1987 commit=032a187f ancestral_da_tag=sim
PR#1989 commit=d330c59f ancestral_da_tag=sim
working_tree=limpa
RECONCILIACAO=OK
```

A validação automatizada das referências confirma a existência dos arquivos/linhas. A conclusão comportamental decorre da leitura do código, não desse contador.

Lição: Valide contratos e configuração efetiva antes de substituir um fork por uma tag.

## Para o dono

**Resultado:** **REPROVADO** para substituir diretamente o fork pela v3.4.2 preservando suas garantias. **EXECUÇÃO: CONCLUÍDA**, com 82 commits reconciliados e quatro dúvidas delimitadas.

**Evidência:** leitura do código da tag, conferência exata dos hashes, ancestralidade dos três PRs e nove testes puros do relay aprovados. Integração com servidor e migração não foram executadas; sockets ficaram bloqueados. Nenhum dado real foi acessado e nenhuma alteração foi feita.

**Conclusões:** não é necessário transportar 82 patches. Os blockers mais claros são endpoint de query, contrato de leitura/ausência/CAS do relay e recibos/drain de hooks. Dream local pode esperar com o agendamento desligado. As correções principais de lápide/restauração e dependências já estão presentes.

**Próximos passos — lista mínima, em ordem:**

**Impedem aprovar a subida equivalente:**

1. Portar `4cfbdf60`: preservar o endpoint separado de queries. Confirmar separadamente se são necessários timeout não padrão e embedding integral; `dream.enabled=false` não responde isso.
2. Portar os restantes de `143ad28a` e `d0eb4754`, mais a adaptação de leitura integral: ausência tipada, corpo ligado ao `page_id`, guards de arquivo e verificação sem truncamento.
3. Portar `eb1ba452` e `d009b152`; conservar o worker local de recibos e regenerar as integrações para impedir drain concorrente.
4. Preservar os guards aplicáveis de `d6db29ad` e scrub antes do corte de `eebdd33c`/`d6a28c77`, com testes adversariais de escopo e controle legítimo.
5. Qualificar o conjunto em ambiente sintético gravável: integração do relay, concorrência/CAS, recibos, backup/restore e migrações. O receipt OKF existente não substitui a preparação de rollback desta atualização.

**Desejáveis, sem necessidade de transportar tudo antes da partida:**

1. Fechar os pequenos restantes de redirects e terminal: `21b7879c`, `d0d13651`, `404fa8a0`, `8f272d97`, `b1389f55`, `56758c2c`.
2. Portar a exclusão exata do projeto de `c4ea6bb3`; resolver as outras mudanças de ranking com fixtures do observation stream ligado/desligado.
3. Recuperar conforme uso: timeout/cache/integral de embedding, pendência tipada de handoff, `from_session_id`, tool-label, descoberta de hooks, limites de recent e reparação de paths.
4. Adiar dream local e curadoria até precisar das operações manuais; manter seus campos locais identificados como ignorados na tag.
5. Adiar o restante macOS se o alvo continuar sendo somente o servidor Linux; aproveitar os testes e scripts locais após adaptação.

VERMELHO


## Porte fase A — execução nesta worktree

A classificação acima é o relatório anterior ao porte. Suas afirmações de árvore limpa, ausência de arquivos e reprovação da tag pura são históricas. Esta seção registra a implementação atual; o relato final nativo continua sendo a autoridade do veredito.

Base: `v3.4.2` / `b9453c96`; ramo `subida/porte-3.4.2`. A classificação foi incluída no primeiro commit, `36e17769`. Nenhuma migração aplicada foi reescrita, nenhuma nova migração foi necessária e a política de `.serve.lock`/receipt pré-migração ficou como na tag. Em serve.rs, somente o clone das configurações dream foi necessário porque scopes deixou de ser Copy.

O primeiro ensaio HTTP, com o release em HOME/data/pre sintéticos e porta
59123, confirmou create_only novo, page_not_found, scope_not_found e
expected_page_id obsoleto recusado sem escrever. Também identificou a leitura
truncada: o relay imutável omite max_chars e recebeu 12038 caracteres de um
corpo de 70015. O dono decidiu preservar o contrato do fork: max_chars omitido
retorna o corpo integral, truncated=false e nenhum marcador; um valor explícito
mantém os limites 500–64000 e o marcador da tag.

Essa correção está em `d8ec45c8`. O teste HTTP
`stateless_read_page_preserves_large_body_without_max_chars` grava 70015
caracteres UTF-8 (140015 bytes), verifica os limites explícitos e compara os
bytes completos na leitura sem parâmetro. Retirando somente o novo tratamento
do parâmetro omitido, falhou com 24038 bytes recebidos; restaurado o porte,
passou. As descrições MCP, a ajuda, a skill distribuída e a documentação do
SDK refletem o contrato. O relay não foi alterado nesta continuação.

Os pendentes herdados foram conservados: `12e2e599` documenta os endpoints,
timeout, embedding integral e redirects; `34e923a3` prova que o drain TS não
envia POSTs nem altera envelopes existentes no spool.

As três dúvidas de ranking foram medidas: título procedural afogado, força relativa de streams, empates e união de scopes com observation_stream=false/true. Portaram-se os comportamentos faltantes e o snapshot comum; extrações de módulos sem efeito adicional não foram copiadas. O teste de snapshot executa um writer SQLite entre as janelas e compara a versão capturada com a leitura posterior. Na fase A, a tentativa extra de mutação negativa desse snapshot foi cancelada antes de executar. A fase C executou a mutação e o controle positivo, registrados na tabela abaixo.

### Fora do binário: decisão individual

| Commit local | Destino ou motivo |
|---|---|
| `bb48faa8` | 87941aeb: runtime Python, launcher, modelos de units e patch consumido pelo staging; sem instalar units. |
| `d7d01799` | 87941aeb: contratos ativos do relay; patch antigo do backend omitido porque a correção está no servidor. |
| `c4d99deb` | 87941aeb: validação de envelopes/schema no runtime Python. |
| `18903744` | 87941aeb: protocolo e patch lido pelo staging; nenhuma aplicação no instalador real. |
| `4a9564e5` | 87941aeb: validação de argumentos no runtime Python. |
| `b753a938` | Documento histórico não copiado; guia do contrato atual escrito em d3b5c794, incluindo o bloqueante de leitura integral. |
| `30ce45eb` | Não portado: corrige patch Python/backend histórico; o servidor foi corrigido diretamente em d3b5c794. |
| `67e370b8` | 87941aeb: correção de título no runtime protocol.py; teste de binário legado não é runtime e ficou fora. |
| `26c60382` | Não portado: fixture histórica do instalador não é consumida pelo runtime instalado e não representa a tag. |
| `613d5b02` | 87941aeb: patch lido pelo staging conservado, sem aplicar o procedimento antigo. |
| `6fe382f3` | 87941aeb: patch de staging conservado; fixture/testes/procedimento antigos não são runtime. |
| `8c107fa4` | Não portado: documentação histórica de bootstrap, sem consumidor no runtime instalado. |
| `d25ef35f` | Não portado: changelog local histórico; seções publicadas da tag ficaram intactas. |
| `63cfcb2d` | Não copiado: inventário histórico; guards atuais documentados com seus testes em docs/security-boundaries.md. |
| `c34c2c99` | 87941aeb: fsync e persistência do outbox em outbox.py. |
| `3b38f76b` | 87941aeb: recuperação offline condicionada ao head em deliver.py. |
| `a9735837` | Não portado: teste de partida de binário legado, não consumido pelo runtime instalado. |
| `00a7b77a` | 87941aeb: worker, recibos completed_at e modelo de unit; ensaio sintético com SQLite. |
| `be5b467b` | 87941aeb: mesmo FD do drain lock durante toda a vida do worker. |
| `c2b0a77d` | Não copiado: teste legado sem consumidor runtime; CAS atual tem recusa de ID de outro projeto e controle legítimo. |
| `d625cb55` | Não portado: procedimento de release/cargo-deny, não usado na execução do servidor instalado. |
| `49ed49e5` | Não portado: helper manual de registro de manifest; não é lido no startup e não atualiza automaticamente o manifest. |
| `ff4682e6` | Não portado: supressões de lint de traits, sem comportamento runtime; gate atual decide a necessidade. |
| `0b9f2599` | Não portado: sintaxe de helper de teste antigo, sem consumidor runtime. |
| `e4624c25` | Não portado: changelog histórico; extensão atual documentada no Unreleased e em a988bb90. |
| `2c24edda` | Não portado: precisão de assert de teste antigo; não muda parser/runtime. |
| `59a33000` | Não copiado: comentários/docs históricos, sem consumidor runtime; aviso de drain desabilitado está no README local atual. |
| `1542b29f` | Não copiado: documentação histórica; retenção nativa de sete dias continua da tag e está no README local atual. |
| `ebb36881` | Não portado: cobertura adicional de lápide, não código runtime; os guards equivalentes da tag foram preservados. |

### Reconciliação final de portar/duvida

| Commit local | Classe | Commit novo ou motivo |
|---|---|---|
| `c4ea6bb3` | portar | `b8409e4a` |
| `bd524699` | portar | `3e87cac2`, `992a3337`, `9c9602a7`, `3fda82ca`, `660fce42`: clientes locais, leitores limitados, snapshots/CAS e operação manual na arquitetura da tag. |
| `585ab238` | portar | `660fce42`: CLI/admin manual reutilizam autorização bootstrap/multiusuário da tag; nenhum gate root redundante. |
| `60593c0b` | portar | `3fda82ca`: gate semântico local com prova literal e negativa; perda de keep-token é recusa. |
| `36cba6dc` | portar | `992a3337`, `3fda82ca`: expiração idêntica (inclusive ausência) antes de inferência e no CAS, sem prorrogar TTL. |
| `3a4a8d79` | portar | `f8509fa0`, `87cc3103`, `96eca2e2`: relações exatas, curadoria condicional, metadados e reconciliação de sessões/diagnósticos. |
| `5cc3d4eb` | portar | `f8509fa0`, `96eca2e2`: sentinelas exatas sem apagar alvo legítimo, preservando declarações brutas e corpo sob head/hash. |
| `d6db29ad` | portar | `09c36478` |
| `4cfbdf60` | portar | `36e17769` |
| `80dd19cd` | portar | Excluído expressamente desta fase: somente macOS. |
| `143ad28a` | portar | `d3b5c794`, `d8ec45c8`: fonte/ID/guards e leitura integral sem max_chars portados; limites explícitos preservados. |
| `d0eb4754` | portar | `d3b5c794` |
| `f3f00446` | portar | `05006c8b` |
| `eb1ba452` | portar | `87941aeb` |
| `1f0ff776` | duvida | `32808d2e` |
| `ad3d8c73` | duvida | `32808d2e` |
| `d2a57a10` | duvida | `32808d2e`; somente snapshot/centralização necessários, sem as extrações rrf.rs/fts_windows.rs. |
| `0511bba4` | portar | `31067278` |
| `0379a6fa` | portar | `e75a95ee` |
| `9cfa8dc3` | portar | `e75a95ee` |
| `eebdd33c` | portar | `3c72db2b` |
| `d6a28c77` | portar | `3c72db2b`, `4b91ecf5`. Contrato do fork preservado: o download mantém o symlink e substitui atomicamente seu alvo regular, conservando permissões; recusa diretórios e alvos com hardlinks sem alteração nem temporário. A suíte da tag foi adaptada: não se espera mais trocar o link por arquivo privado nem deixar o alvo regular intacto. |
| `a032e4c6` | duvida | Não portado: `cargo clippy --workspace --all-targets -- -D warnings` passou com Rust 1.95, sem precisar das supressões de dream/curadoria ou helpers SHA antigos. |
| `29549016` | portar | `3581a6fd` |
| `1d93add8` | portar | `a988bb90` |
| `62baa09e` | portar | `a988bb90` |
| `59873022` | portar | `fd11ef8c` |
| `985e96d2` | portar | `c89826fa` |
| `f3fb8aac` | portar | `d949cd0a` |
| `b078dcb1` | portar | `23cd7ee7` |
| `c3b5a60e` | portar | `55158975` |
| `2d26dae5` | portar | `5fe0e7cc` |
| `d009b152` | portar | `87941aeb` |
| `21b7879c` | portar | `4b91ecf5`; redirects continuam recusados. O contrato de destino compartilhado nesse porte segue `d6a28c77`, com symlink preservado e recusa de alvo não regular ou com hardlinks; substitui a expectativa de troca do symlink da tag. |
| `d0d13651` | portar | `4b91ecf5` |
| `404fa8a0` | portar | `4b91ecf5` |
| `8f272d97` | portar | `e88899c4` |
| `b1389f55` | portar | `e88899c4` |
| `56758c2c` | portar | `e88899c4` |


### Validação da continuação

Toolchain usada: Rust/Cargo 1.95.0. O shell inicial apontava para 1.99.1;
os comandos Cargo foram executados com o diretório da 1.95 no início do PATH.

- `cargo build --release --locked`: passou.
- SHA256 de `target/release/ai-memory`:
  `94b9c6564c417c701bb3e08a96b9ad6d9d4485067ceafdfe40daa0d686c53359`.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo fmt --all -- --check`, `git diff --check` e cargo-deny offline:
  passaram.

Foram executados `cargo test --workspace --exclude ai-memory-cli --no-fail-fast`,
`cargo test --locked -p ai-memory-cli --lib` e os doctests CLI: 7375 passed,
0 failed, 19 ignored no conjunto dos comandos. Os três ignorados da biblioteca
CLI foram executados depois com `--ignored`: 3 passed, 0 failed. O guard
instalado `assert_fixbrainrank.sh` confirmou exatamente 1 passed.

| Crate | passed | failed | ignored no comando padrão |
|---|---:|---:|---:|
| ai-memory-cli (lib) | 2346 | 0 | 3 |
| ai-memory-consolidate | 569 | 0 | 6 |
| ai-memory-core | 385 | 0 | 0 |
| ai-memory-eval (lib + smoke) | 84 | 0 | 4 |
| ai-memory-hooks | 804 | 0 | 0 |
| ai-memory-llm | 453 | 0 | 2 |
| ai-memory-mcp | 847 | 0 | 2 |
| ai-memory-store | 958 | 0 | 1 |
| ai-memory-web | 311 | 0 | 0 |
| ai-memory-wiki | 383 | 0 | 1 |
| ai-memory-workstream | 235 | 0 | 0 |

Os demais ignorados são testes de modelo/dataset/provedor externo ou medições
manuais. Os 32 testes Python do worker passaram. A comparação dos plugins
commitados com o renderer do release e a renderização das cinco integrações TS
em diretórios sintéticos também passaram.

Cada obrigatório teve prova por remoção do trecho correspondente, falha de
asserção e restauração: 19 pares, cada um com 0 passed/1 failed sem o trecho e
1 passed/0 failed com ele. A leitura integral acrescenta o par HTTP descrito
acima. Os hashes dos fontes restaurados foram conferidos. Os comandos e logs
estão em `target/evidence-a3/negative-summary.json` e nos arquivos adjacentes;
erros de compilação ou do harness não foram contados como prova negativa.

O release foi exercitado com o cliente original do relay, sem alterá-lo, em
HOME/data/pre sintéticos e porta 59123. Passaram create_only, page_not_found,
scope_not_found, recusa do expected_page_id obsoleto sem alterar arquivo nem
quantidade de versões, descrição/ajuda publicadas, leitura integral de 70015
caracteres/140015 bytes e caps explícitos. Um segundo servidor sintético
confirmou documento integral, endpoint de consulta separado, identidade do
embedding e cache de query com dream desligado e scopes não vazio.

**Fechamento pendente:** não foi executado `cargo test --workspace` sem
exclusões. A suíte de integração CLI compilou e listou 430 testes; somente os
4 testes estáticos de repo_layout foram executados, todos aprovados. Restam
426 sem execução. Seus launchers herdam/modificam o ambiente e alocam portas
com `127.0.0.1:0`, sem garantir o env-i e o piso >59000 exigidos pelo brief.

O primeiro ensaio CLI usou TMPDIR dentro da worktree e teve 50 falhas porque
fixtures sem repositório encontraram o Git/configurações ancestrais. Após
corrigir o ambiente, a biblioteca passou sem alteração do código. O marcador
sintético criado na raiz foi transferido para as evidências. O log também
registra descoberta da configuração .claude do checkout principal; esse
ensaio não certifica a fronteira de principal intocado, e não foi feita nova
inspeção dele. Por essas lacunas, esta continuação não libera VERDE para o
fechamento da fase A. O relato final nativo permanece a autoridade do veredito.


## Fase B — decisões de reconciliação

| Origem | Equivalente preservado na tag/fase A | Garantia acrescentada e prova |
|---|---|---|
| `3a4a8d79`, `5cc3d4eb` — parser | `split_scope`, vocabulário `Relation`, `PagePath` e roteamento `_global` | Relações exigem `.md` explícito; títulos e sentinelas nus permanecem no frontmatter, sem inventar caminho. Formas `.md` de sentinela continuam candidatas a alvo legítimo. Regressões `markdown::tests::curation_*`; negativo do parser reprovou antes da correção. |
| `80dd19cd` | Fora do servidor Linux | Excluído também da fase B: alteração exclusiva do companion macOS. |
| `bd524699` — clientes | Helpers da tag para endpoint compat, leitura de resposta limitada, schema estrito e wrappers de reasoning | Cliente dedicado sem credenciais/proxy/redirect/retry/fallback, orçamento de entrada/saída e deadline incluindo corpo. `LlmError::Provider` conserva `retry_after`; embedding da fase A não foi alterado. Testes `dream_local` (10 casos) e runner de mutações em `target/evidence-b/local-clients/`. |
| `3a4a8d79`, `5cc3d4eb` — store | `upsert_page_in_tx`, ator único, supersessão e leitor de links da tag | Aresta tipada só entra com alvo exato existente na transação; referências do corpo mantêm forward links. Metadados exigem tuple/head/hash armazenado e hash do corpo recebido, preservando autor, TTL, evidência e vetores compatíveis. Fixtures de grafo mantiveram os controles de TTL/lápide e passaram a exigir ausência de aresta para alvo que nunca existiu. |
| `3a4a8d79` — sessões e diagnósticos | `reconcile_session_consolidation_completed`, leases e `redacted_error_summary` | Reconciliação também liga o anchor latest da própria sessão no escopo resolvido e registra `no_session_page` quando ausente, sem terminar sessão nem consumir lease `running`. Foi reutilizada a redação de erros da tag, mais estrita que o helper do fork. Testes metadata/multi_session/session_consolidation e controles de escopo/autor. |
| `3a4a8d79`, `5cc3d4eb` — curadoria e geração | Curator genérico permanece report-only; fonte/index/locks, admissão e rollback da Wiki foram reutilizados | Foi acrescentado `/admin/curate-relations`, dry-run por padrão, head+hash e recibo sem corpo. Só sentinelas exatas são removidas; `.md` existente é preservado, nenhuma página-alvo é apagada. Mudança do corpo por sanitizer/webhook recusa a operação. Declarações brutas do consolidator permanecem no frontmatter; só a aresta sem alvo é recusada. Testes Wiki 13/0, CONS 3/0 e MCP 3/0 focados. |
| `585ab238` — acesso administrativo | `require_root_for_multiuser_admin` e `Capability::Admin` da tag já cobrem bootstrap e fechamento após primeiro usuário/proxy | Reutilizado o middleware operacional, sem novo check que bloqueie instalação anônima de um operador; testes dos novos endpoints cobrem bootstrap, root, DB-user, anônimo e campos desconhecidos. |
| `bd524699`, `36cba6dc` — store dream | `row_to_decay_candidate`, `upsert_page_in_tx` e ator único da tag | Novos leitores limitados recusam cap+1 e dimensões inválidas, sem sampling; CAS transacional exige coordenadas de todas as fontes, head, hash do corpo e digest de elegibilidade. TTL precisa permanecer idêntico, inclusive ausência, e não pode expirar até apply. Cancelamento antes de commit faz rollback de todas as versões. Testes `dream_cas` (10 casos) com controles de identidade e hash independentes. |
| `bd524699` — snapshots/CAS | Clustering, cold scoring, `apply_batch_inner`, `PagePrecondition`, fase A OS locks, rollback e supersessão | Leitores limitados de heads/vetores, hash completo de arquivo/corpo, digest de elegibilidade e CAS de todas as fontes com TTL idêntico foram acrescentados ao funil existente. Snapshot não cria manifests. Checkpoint só das fontes conserva também confinamento de `.git` da tag, ausente no helper antigo do fork. |
| `60593c0b`, `36cba6dc` — semântica/preservação/TTL | B4 e `LlmOperationId(Dream)` da tag; limpeza de summary/abstract nos stubs | Jev local decide relação temática; suporte é provado por duas cópias completas exatas e título literal. Warning de keep-token e delink do draft viraram recusa, preservando negação, exceção, conflito, números e bindings. `dropped_keep_tokens` legado permanece vazio. Expirações comparam instantes, inclusive ausência, antes de inferência e na transação. |
| `bd524699` — atividade/deadline | Clock do tool router e cadência nativa da tag | Clock ganhou reservas/generation para hooks pós-202, tools, embeddings e jobs relevantes; scheduler e manual usam o mesmo runtime serializado e prazo global. Espera de OS locks usa try-lock assíncrono: a revisão reproduziu um worker bloqueante retendo lock após cancelamento; teste vermelho/verde prova a liberação mantendo o segundo lock preso. Shutdown/desconexão drenam apply/rollback antes de fechar o store. |
| `bd524699`, `585ab238` — manual/allowlist/config | Scheduler da tag e política admin operacional foram mantidos | Foram acrescentados CLI/endpoint manual, allowlist exata compartilhada, config estrita e modelos dedicados. Manual força apenas o seu pass, sem ligar agendamento. `dream.scopes` não vazio continua selecionando embedding `local_only` com `enabled=false`; regressão da fase A e forma de produção estão cobertas. |
| `3a4a8d79`, `5cc3d4eb` — orçamento de prompt | Piso de entrada, reserva de observações e oracle independente de projeção da tag | `65404ff0`: orientação de relações exatas foi compactada para não consumir o piso existente. O oracle recebe apenas a nova redação estática, mantendo projeção e seleção congeladas; regressões de reserva e equivalência byte a byte foram preservadas. |

### Prova de fechamento da fase B

Foi usado Rust/Cargo 1.95.0, com o diretório da toolchain no início do PATH,
`TMPDIR=/tmp/x.HTgVMK` fora dos repositórios e HOME/XDG sintéticos nos testes
completos. O wrapper de testes não executa a suíte de integração CLI.

`cargo build --release --locked`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all -- --check`, `git diff --check` e cargo-deny offline passaram.
Clippy, fmt e diff foram repetidos após todas as mutações negativas.

SHA256 de `target/release/ai-memory`:
`519e049d5ba736b227dae3218fecdc613c2b18ba6ce01c15f96beb45a00ccec6`.
Os hashes de todos os fontes restaurados coincidem com os usados no build.

| Crate (`cargo test --locked -p ...`) | passed | failed | ignored |
|---|---:|---:|---:|
| ai-memory-cli (somente `--lib`) | 2352 | 0 | 3 |
| ai-memory-consolidate | 600 | 0 | 7 |
| ai-memory-core | 385 | 0 | 0 |
| ai-memory-llm | 463 | 0 | 2 |
| ai-memory-mcp | 859 | 0 | 2 |
| ai-memory-store | 975 | 0 | 1 |
| ai-memory-wiki | 410 | 0 | 1 |
| ai-memory-hooks | 804 | 0 | 0 |

Total medido: 6.848 passed, 0 failed, 16 ignored. Os ignorados pertencem às
categorias de modelo/provedor externo, medição manual e execução das integrações
geradas da fase A; nenhum controle obrigatório da fase B foi ignorado.

Os nove obrigatórios têm 41 pares negativos distintos: cada teste passa antes,
reprova em runtime com a garantia neutralizada (0 passed/1 failed, exit 101), e
passa após restaurar os bytes originais (1 passed/0 failed, exit 0). Erro de
compilação não vale como prova. O resumo com requisitos, comandos, patches,
hashes dos fontes e logs é `target/evidence-b/negative-summary.json`.

O e2e usou o release acima, `env -i`, HOME/data/pre sob mktemp externo,
porta alta 61000 e servidores locais sintéticos. Com `enabled=false`,
o plano não fez inferência nem alterou as fontes; apply reuniu duas cópias
literais, preservou histórico/evidência e recusou o escopo externo com HTTP 403.
A forma de produção de `[dream]` foi aceita; campo desconhecido foi recusado.
O teste de embedding e seu negativo provaram que scopes não vazio mantém
`local_only` com o scheduler desligado. Evidência: `manual-e2e-result.json`,
`crate-test-results.json`, `final-gate-results.json` e `built-source-hashes.json`
em `target/evidence-b/`.

Antes e depois dos testes, `git -C /home/marce/Projects/ai-memory status --short`
e `ls -la /home/marce/Projects/ai-memory/.claude` produziram exatamente o mesmo
resultado: as alterações preexistentes de outra frente foram preservadas e
`.claude` continua sem `settings.local.json`. Snapshots: `main-before.json` e
`main-after.json` no mesmo diretório de evidências. Não houve migração, alteração
do relay, uso de dados/serviço/portas protegidos nem push; `80dd19cd` permanece
excluído. O relato final nativo é a autoridade do veredito.


## Fase C — correções da revisão cruzada

As revisões a3 e b foram lidas integralmente como evidência e comparadas aos
objetos do fork; a base desta fase é `c0daec8d`. Não há alteração de schema.

| Item / origem | Decisão e contrato reconciliado | Prova desta corrida |
|---|---|---|
| F1 / `3a4a8d79` | Reconcile no consolidator conserva summary e falhas antigas, mas limita o UPDATE à menor geração `running`; gerações posteriores continuam pendentes. Sem lease ativo, o reconcile manual mantém o contrato anterior. Invariante #16: nenhum lease é consumido ou invalidado e todas as escritas continuam no ator único. | `0b7ed5f4`: `automatic_reconcile_preserves_newer_pending_generation`: A running, B pending, consolida A, completa A, claim B e segunda inferência. Sem o limite SQL: falha runtime; com ele: passa. Controles existentes preservam failed antiga e lease ativo. |
| B1 / `2d26dae5` | Checkout exige Cargo.toml com `[workspace]` e `hooks/claude-code/`. Só o exe sob `checkout/target/` torna esse checkout primário; cwd e manifest vêm após sibling, prefixo, sistema e data dir. `--hooks-dir` continua absoluto na precedência. Caminho compilado removido é ignorado. | `4b97ced0`, `35ad6823`: `installed_hooks_bundle_precedes_a_cwd_checkout` falha no runtime antigo e passa com a nova ordem; controla exe sob target e override explícito. Fixtures de triple, symlink e bundle sibling preservadas com marcadores reais. |
| C1 / `143ad28a` | Locks OS usam hash da tupla módulo 1024, com prefixo `pool-v1-`; colisões só serializam páginas distintas. Aquisição continua ordenada e deduplicada. Arquivos SHA antigos ficam inertes, sem remoção. Instâncias novas devem usar o mesmo binário; um escritor antigo simultâneo usaria outro conjunto de locks. | `cb4189a4`, `e0b345de`, `35ad6823`: 1100 leituras de caminhos inexistentes: antes criam 1100 locks, depois no máximo 1024; o arquivo legado semeado conserva os bytes. Controles de escritores independentes, CAS e cancelamento continuam usando o pool. |
| C2 / `d009b152`, `c3b5a60e` | Os dois controles Node ignorados agora estão no `--ignored --exact` do CI. O runtime TS perde todo o drain morto, inclusive unlink de envelopes; integração externa semeia envelope e exige bytes intactos. | `07ec2cce`, `32c55497`: Negativo executado antes da remoção do código morto: retirar só o return apaga o envelope e falha com ENOENT. O teste Node final verifica envelope intacto, zero POST de backlog e controle de escrita; teste estático proíbe o unlink. A suíte CLI de integração não foi executada nesta corrida, conforme fronteira do dono. |
| C3 / `d009b152` | O worker inalterado aceita o contrato real dos produtores shell e PowerShell com escopo/key e auth none/static. Aplicado o caminho TS: drains explícitos e piggyback dessas libs são no-ops; só worker (ou drain nativo com lock/recibo, quando worker não está ativo) retira backlog. Perfis/OIDC/escopo implícito continuam fora do suporte desse worker e ficam para o nativo; não ampliamos o contrato. | `41024a24`: `test_spool_worker.py` executa os dois produtores, prova entrega, preservação sem completed_at e arquivo+recibo antes da remoção. pwsh disponível e exercitado. `test_lib.sh` tem negativo: 2xx apagava envelope; passa com no-ops. Controles antigos de entrega do drain script foram invertidos explicitamente para retenção de todos os modos; marcadores/proxies continuam testados nas requisições restantes. |
| F2 / tag e `c0daec8d` | Fechamento de backfill usa o mesmo `base` efetivo e `.routed_to(profile_name)` dos eventos normais; o token do perfil não viaja ao servidor default. | `574b71f9`: `backfill_close_uses_the_claim_profile_and_a_fresh_key` exercita session-start/handoff com dois servidores sintéticos: antes o envelope nomeia o default; depois nomeia URL e perfil team-b. Nenhum pedido chega ao default. |
| N1 da revisão b | Guarda local recusa fechamento sem scope_qs e sem cwd não branco, mesmo se chamado fora do fluxo atual. A chave do fechamento é nova por evento. | `574b71f9`: `backfill_close_without_scope_or_cwd_is_not_emitted` cobre None/vazio/branco e controle com cwd. O teste de perfil compara a chave do fechamento com a chave do session-start realmente enviado, ambas com 32 caracteres e distintas. |
| C5 / `404fa8a0` | Política custom continua recusando todo redirect. Mudança de origem retorna o diagnóstico da tag com a origem alvo, sem replay; mesma origem continua 3xx recusado. Dream mantém mensagem privada e cliente sem proxy/redirect. | `15773fee`: Quatro asserts da tag restaurados (run e notify, texto e origem alvo); `Policy::none()` falha no primeiro. A política corrigida passa junto com os controles de não envio, Ignore e recusa na mesma origem. Comentário do notify atualizado. |
| C4 / `8f272d97`, `b1389f55`, `56758c2c` | Saneamento já portado ganhou testes nos quatro sinks: erro HTTP do finalize, ambas as mensagens de tag do upgrade, sugestão de agente do doctor e menu com nome de workstream. Helpers privados de mensagens permitem testar a redação usada pelo println. | `dfd6f9d3`: `terminal_error_body_is_single_line_and_control_free` falha com terminal_body (newline/tab chegam ao erro) e passa com terminal_text. Os outros três controles incluem ESC, DEL, C1, CR, tab e quebra de linha com controle de texto comum; doctor/menu mantêm apenas as quebras da própria apresentação. |
| Ranking / `1f0ff776`, `ad3d8c73`, `d2a57a10` | Nenhuma mudança adicional no ranking. Executada a mutação que faltava: trocar a transação comum por leituras autocommit em `merge_field_windows`. Fonte restaurado byte a byte depois do ensaio. | `e29f54f1`: `cargo test --locked -p ai-memory-store field_windows_share_snapshot_during_concurrent_writer`: sem snapshot, 0 passed/1 failed (duas versões, esperado uma); restaurado, 1 passed/0 failed. Logs `RANK-without.log` e `RANK-with.log` em `target/evidence-c/`. |
| Idioma do dream / tag → `3fda82ca0` | Registrada a retirada dos três asserts de `dream_prompt_mirrors_page_language`: `## OUTPUT LANGUAGE`, `dominant natural language of the pages` e `do not make English the output language`. O prompt passou a exigir cópia literal integral das fontes e título literal, como o fork; tradução ou reescrita é recusada pelo validador determinístico. Não reinstalamos as instruções antigas de geração livre. | `git show 3fda82ca0 -- crates/ai-memory-consolidate/src/dream.rs`; controles de preservação literal e keep-token permanecem no consolidate. |
| N1 da revisão a3 — nome de ferramenta / `c3b5a60e` | Reversão explícita do contrato da tag: `tool_label` validado é gravado no corpo por padrão, inclusive metadata-only; `record_tool_names` controla o título. Documentação de config corrigida. | Parser/gramática fechada, payload metadata-only e teste Node de labels; não é autorização para gravar argumentos/saída. |
| N1 da revisão a3 — redirect / `404fa8a0` | Reversão explícita: redirects na mesma origem também são recusados, sem replay; C5 recupera o diagnóstico entre origens sem reabrir o follow. | `same_origin_redirect_never_replays_the_webhook`, `redirect_credentials` e controles de C5. |
| N1 da revisão a3 — leitura / `143ad28a` | Reversão explícita: `max_chars` omitido retorna a página integral, sem teto; limites explícitos continuam aplicados depois da comparação integral. | `relay_read_is_complete_without_max_chars` e negativo da fase A; contrato preservado nesta fase. |
| N1 da revisão a3 — cache / `d6db29ad` | Reversão explícita: respostas privadas usam `Cache-Control: private, no-cache`; cada revalidação repete a autorização e enxerga revogação. | Teste de grant/cache da fase A permanece no workspace; não restauramos max-age da tag. |
| Higiene | Corrigido o comentário de dream que dizia de-link em vez de recusa; `target-host/` ignorado. Nenhum arquivo de build foi incluído no git. | fmt, diff-check e inspeção final do status. |
| C1 — controle de cancelamento | A fixture escolhe dois slots distintos mesmo quando os IDs aleatórios fazem a.md e b.md colidirem; colisão é válida no pool e não significa que o primeiro lock foi adquirido. | Mantido o mesmo oráculo: cancelamento libera o primeiro slot enquanto o segundo segue preso por um escritor externo. |
| C2 — consumidores gerados | OpenClaw e os quatro geradores TS deixaram de exigir a presença do drain removido nos asserts legados; agora exigem requestSpoolDrain compatível e ausência de drainHookSpool/unlinkSync. Import de unlinkSync retirado do runtime gerado. | Controles de escrita de spool, fila limitada, abort e fs imports úteis permanecem. |
| C2 — oracle compartilhado | O oracle TS ainda exigia o header e a requisição do drain morto. Substituídos por recusa de drainHookSpool/unlinkSync e requestSpoolDrain inerte. O mínimo de fetches cai de três para dois (hook e contexto); cada fetch restante continua exigindo authHeaders/mcpHeaders, sem proxy, sem redirect e sem injetar erro de contexto. | Os controles de escrita, chave, seq, abort e classificação retryable continuam estritos; removida a alternativa de headers brutos que só o drain usava. |
| Gates — ajustes de fixtures | O gate completo encontrou a fixture cargo-run de setup-agent sem o novo marcador exigido por B1; acrescentado Cargo.toml de workspace sem mudar o assert de descoberta. Clippy exigiu array no controle de dois slots de C1. | Reexecutados os gates após corrigir as duas fixtures; nenhum prazo ou assert foi afrouxado. |

### Prova de fechamento da fase C

Resultado: APROVADO nos gates pedidos, execução concluída. Todos os itens
1–11 foram atendidos. Rust/Cargo 1.95.0, com TMPDIR externo
`/tmp/x.wFz8g0`, HOME/XDG sintéticos e configuração Git isolada nos testes.

Passaram `cargo build --release --locked`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --check` e `git diff --check c0daec8d..HEAD`. O ledger em
`target/evidence-c/GATES.md` terminou com 18/18 gates satisfeitos, nenhum
pendente ou abandonado. Os primeiros logs de falha de fixture/clippy foram
preservados em arquivos `*-attempt1.*`; a execução final passou integralmente.

SHA256 de `target/release/ai-memory`:

```text
33b9f1ff4cfb3a5dedb7bcc483c9e42dc33bd8690f6d7122e4b1151691149603
```

Execução final de `cargo test --locked -p <crate>` (CLI apenas `--lib`):

| Crate | passed | failed | ignored |
|---|---:|---:|---:|
| ai-memory-cli (somente lib) | 2361 | 0 | 3 |
| ai-memory-consolidate | 601 | 0 | 7 |
| ai-memory-core | 385 | 0 | 0 |
| ai-memory-hooks | 804 | 0 | 0 |
| ai-memory-llm | 463 | 0 | 2 |
| ai-memory-mcp | 859 | 0 | 2 |
| ai-memory-store | 975 | 0 | 1 |
| ai-memory-wiki | 411 | 0 | 1 |

Total medido: 6859 passed, 0 failed, 16 ignored. Os três
ignorados obrigatórios do render_shared foram executados separadamente com
`cargo test --locked -p ai-memory-cli --lib commands::render_shared -- --ignored`: 3 passed.
Os demais ignorados continuam sendo controles externos/manuais já existentes.
A suíte CLI `--test suite` não foi executada, conforme a fronteira do dono.

`sh tests/hooks/test_lib.sh`: 442 passed, 0 failed. O teste de compatibilidade
executou os produtores shell e PowerShell (pwsh disponível), com auth none e
static, contra o worker original; dois testes aprovados, quatro combinações.
`python3 -m pytest local-reliable-mcp/tests/test_hook_delivery.py`: 32 passed,
com PYTHONPATH apontando ao módulo local, sem alterar o worker/relay.

Nove pares negativos de runtime cobrem os itens 1–6 e 8–10.
`target/evidence-c/negative-summary.json` contém os comandos, mutações,
asserções, saídas e hashes dos logs: todos reprovam sem a correção e passam
com ela. Falhas de compilação/harness iniciais não contam como negativos.
O negativo TS foi executado retirando só o return antes da remoção do código
morto; o envelope semeado foi apagado. O negativo de ranking expôs duas
versões, depois voltou a uma; reader.rs foi restaurado byte a byte.

Evidências complementares no mesmo diretório: `crate-test-results.json`,
`final-summary.json`, `gates-final.log`, `built-source-hashes.json` e
`release-sha256.json`. Os fontes finais coincidem com o snapshot dos gates/build.

Antes e depois dos testes, `git -C /home/marce/Projects/ai-memory status --short`
e `ls -la /home/marce/Projects/ai-memory/.claude` permaneceram idênticos
(`main.before/after` e `claude.before/after`).
`git diff --name-only c0daec8d..HEAD | grep -i migration` saiu vazio.
Sem instalação, push, mudança de serviço, dados reais ou acesso aos caminhos
e portas protegidos. target-host foi apenas ignorado, sem remoção.

O orquestrador pode executar a suíte CLI de integração no host e seguir para
a próxima revisão cruzada. Nenhum item solicitado desta corrida ficou pendente.

## Fase D — checkout primário e reparo de startup

Base: `fd089e10`. A prioridade B1 agora usa o checkout mais próximo do
executável, identificado pelos mesmos marcadores de `install_layout`
(`Cargo.toml` com `[workspace]` e `hooks/claude-code/`). O nome do diretório
de build não participa da decisão. `repo_root_from_dir` já fazia essa busca;
o único teste de produção com `target` literal no `4b97ced0` estava em
`hook_source_candidates` e passou a reutilizar esse helper.

| Achado | Commit | Teste | Log negativo em `target/evidence-d/` |
|---|---|---|---|
| B1 / `4b97ced0`: bundle no data dir ganhava do checkout com target customizado | `412c00c7` | `checkout_hooks_precede_installed_bundle_with_custom_target_dirs`: `target-host/debug`, `target-x/release` e `build/debug`, com bundle instalado presente | `D1-targets-without.log`: escolheu data dir em vez do checkout |
| Checkout aninhado: cwd principal não pode tomar a prioridade da worktree do executável | `412c00c7` | `nested_executable_checkout_precedes_the_main_cwd_and_installed_bundle`: exe em `.worktrees/x/target-host/release`, cwd principal | `D1-nested-without.log`: worktree não ficou primária |
| Guarda contra uma correção ampla demais por prefixo | `412c00c7` | O mesmo teste passa uma raiz externa à worktree; ela permanece depois do bundle instalado | `D1-outer-guard-without.log`: mutação para `starts_with(root)` escolheu `<main>/hooks`; restaurado byte a byte |
| Nota: reparo date-only gravava sem o pool compartilhado | `c556b26a` | `startup_repair_waits_for_an_independent_wikis_pool_slot`: outra Wiki segura o slot, arquivo fica intacto; após soltar, reparo termina | `D2-repair-without.log`: reparo terminou enquanto o slot estava preso |

Os quatro negativos falharam em runtime: 0 passed, 1 failed, exit 101 cada.
Os fontes corrigidos voltaram a passar. `negative-summary.json` guarda os
comandos e hashes dos logs; `mutation-restoration.json` comprova a restauração
da mutação por prefixo. B1 original passou sem alteração, junto do novo
controle de binários runtime, `.local/bin` e `usr/bin` fora do checkout.
As seis fixtures de `install_layout` e o controle existente de lock entre
Wikis também passaram. Foram 13 testes focados distintos.

A nota coube em 6 linhas de produção: `mutation_lock` → lock da página em
memória → slot `pool-v1-*`, antes de ler, interpretar e gravar cada arquivo.
Pelo invariante #16, a chave continua sendo `(workspace, project, path)`;
Wikis independentes compartilham o slot sem filtrar páginas por operador.
O reparo SQL permanece no ator único e não cria versões nem migração.

Validação final com Rust/Cargo 1.95.0, `TMPDIR=/tmp/x.reIfdd`, HOME/XDG
sintéticos e `CARGO_TARGET_DIR=target-host`:

| Comando | passed | failed | ignored |
|---|---:|---:|---:|
| `cargo test --locked -p ai-memory-cli --lib` | 2364 | 0 | 3 |
| `cargo test --locked -p ai-memory-wiki` | 412 | 0 | 1 |

Total: 2776 passed, 0 failed, 4 ignorados preexistentes (três controles Node
da CLI e uma medição manual da Wiki). Não foi executada a suíte CLI
`--test suite` nem o workspace completo. Logs finais: `final-package-cli.log`
e `final-package-wiki.log`. Passaram também clippy dos dois pacotes com
`--all-targets -- -D warnings`, fmt, diff-check e os checks de
seções/congelamento do changelog. A primeira tentativa de fmt pediu somente
a quebra de um assert novo; seu log foi preservado e a validação foi repetida
após formatar.

Sem mudanças em migrações, testes antigos, serviço ou dados reais. Sem push.
O relato final nativo é a autoridade do veredito.
