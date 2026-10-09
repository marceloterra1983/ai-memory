# Integrações sintéticas, sem instalar hooks

O runner exercita o binário explícito em um processo temporário, com HOME,
config, gitconfig e data-dir novos. Não ativa o serviço da máquina, instala
hooks ou acessa stores nativos. Envia fixtures ao `/hook` e usa as ferramentas
MCP reais do mesmo servidor. As gravações passam pelo sanitizer/writer/wiki/
admission normais; a verificação SQL é somente leitura do banco recém-criado.

```bash
# Não instala toolchain nem pacotes globalmente.
bash local-maintenance-quality/rust.sh build --bin ai-memory
python3 -m unittest discover -s local-maintenance-quality/tests -v
python3 local-maintenance-quality/run_lab.py \
  --binary "$PWD/target/debug/ai-memory" \
  --lab-dir /tmp/ai-memory-quality-lab-novo \
  --server-url http://127.0.0.1:49479 \
  --workspace quality-synthetic
```

`fixtures.json` contém 22 casos de wire protocol: 20 agent kinds de captura
(incluindo o adapter genérico `other`), snake/camel/dual casing, Cursor
`workspace_roots`, Antigravity `conversationId`, ponte Devin por query,
Kimi content blocks e três envelopes OpenCode. OpenCode 2 usa o mesmo agent
kind/parser (`AgentKind::from_wire` normaliza `opencode2`/`opencode-v2`/
`open-code2`); a instalação MCP v2 tem seu próprio schema e seus testes existentes.
As fixtures cobrem o protocolo aceito pelo servidor; não executam os CLIs dos
fornecedores nem afirmam que seus hooks estão instalados nesta máquina.

Cada caso verifica três observações, agent attribution, encerramento,
`summary_page_id` apontando a página latest com seu fato exclusivo, recall MCP
daquele path e entrega/consumo único do handoff. A consolidação usada aqui é
`synthesize_session_page`, o caminho determinístico sem LLM. Os testes de worker
do patch principal cobrem separadamente o caminho assíncrono com provider
sintético; este runner não simula uma execução de provider real.

Controles adicionais:

- Sessão apenas start/end: duas observações e nenhuma página ou handoff vazio.
- Muse MCP-only: list, accept por ID exato, replay sem segunda entrega, recusa de
  um ID de outro projeto e recent. Claude Desktop e os demais clientes MCP-only
  usam as mesmas ferramentas; não lhes é inventado um lifecycle hook.
- Discover: raiz git registrada, override explícito de projeto filho preservado
  e `repo_path` do filho nulo, sem criar um catch-all para a raiz.
- Curadoria HTTP: dryrun implícito/noop numa página limpa, anônimo recusado,
  head stale recusado e campo desconhecido recusado. Remoção real de sentinelas,
  CAS e rollback são provados pela suíte Rust `relation_curation` do patch.
- Home Assistant: uma página manual é recuperável por MCP com zero observações;
  isso é um estado válido, não evidência de captura quebrada.

## Origem e equivalência

O draft anterior mapeava `discover` em `router.rs`; a leitura completa mostra
`resolve_project_ids_inner`, `derive_project_name`, `repo_path_from_cwd` e
`repo_path_from_project_override` já protegendo raiz vs. filho. Sem override, um
cwd dentro de uma raiz previamente cadastrada reutiliza legitimamente o projeto
da raiz. O primeiro teste supunha o contrário e foi corrigido no harness; nenhum
defeito no resolver foi demonstrado.

`handle_handoff`/`fetch_and_accept_handoff_at` são o caminho síncrono que resolve
escopo e aceita antes de entregar o contexto. `SessionEnd` gera a passagem
automática; o branch `LifecycleOnlyEndOutcome::Ended` fecha sessões efêmeras sem
inventar artefatos. A documentação e as tool descriptions existentes já indicam
`memory_handoff_list` + `memory_handoff_accept` para clientes sem stdout/hook.
`McpClient::Muse` já é MCP-only em `cli.rs` e `install_hooks.rs`. A correção mínima
é provar esses comportamentos e fornecer o runner, sem alterar ou suprimir warns.

## Evidência e limites

09/10, Rust 1.95 no Docker: build do binário passou. Execução completa exit 0,
27 casos/controles, baseline 11/11 MCP e FTS. Artefato privado:
`/tmp/ai-memory-quality-lab-1009-final-ttl/evidence.json`. Unit tests Python:
5/5 do laboratório e 13/13 na suíte combinada com curadoria.
O lab completo encerrou seu processo. `server.log` fica no diretório privado,
com warns habilitados e somente fixtures sintéticas; não é despejado no terminal.

Red/green: a cópia original do bench falhava por `ask.py` ausente e as fixtures
eram ausentes/placeholders; os testes passaram depois da adaptação isolada.
No runner, a expectativa equivocada de discover falhou de forma observável e
foi corrigida sem editar produção. O teste HTTP inicialmente encontrou a recusa
401 esperada sem root bearer; a fixture agora usa bearer sintético e verifica
também a recusa anônima, stale 409 e DTO inválido 422.

Nada aqui diagnostica logs de produção ou recupera sessões reais. Os cinco IDs
de replay `default/dev` e sua recuperação após checkpoint permanecem com o dono.
O inventário histórico do draft não é tomado como estado atual.
