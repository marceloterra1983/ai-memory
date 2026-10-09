# Curadoria e integrações — construção local 1009

Base `c4ea6bb3` (inclui fixbrainrank), versão workspace 2.5.2 preservada.
O trabalho corrige os gaps de geração de relações e vínculo de resumo, fornece
curadoria exata com CAS/admission e completa o laboratório sintético. Não há
release, deploy, replay real, alteração de hooks globais ou escrita em produção.

Origem: relatório privado `aimemory-maintenance-1009.md` e draft
`memory-maintenance/local-maintenance`. Suas contagens incluem histórico e não
representam latest. Nenhuma proposta clínica daquele inventário foi lida.
Os defeitos foram reproduzidos contra fonte e fixtures sintéticas; a curadoria
não renomeia, infere, funde ou apaga conhecimento por similaridade ou título.

- [Curadoria](curadoria/README.md): helper dry-run padrão, SQL somente leitura,
  root admin com DTO estrito, no-create, expected head/hash, body/frontmatter
  protegidos, versão anterior/evidence/vetores e checkpoint recuperável, sem
  chamada a provedor de embeddings. QLMED só contagem.
- [Integrações](integracoes/README.md): runner real de lifecycle, capture,
  consolidação determinística, recall, discover e handoff; Muse MCP-only e Home
  Assistant com páginas/zero observações são controles válidos.
- [Baseline](baseline/README.md): cópia isolada do bench, escopos
  regente/meusagentes, 11 gabaritos sintéticos executáveis e comparação MCP/FTS.

O parser preserva relações ambíguas e origem no frontmatter; o writer só promove
relações tipadas resolvidas em sua transação. Não é necessário Jev: sentinelas,
paths exatos existentes, scope, head e hash são decisões determinísticas. O
worker e os caminhos sync/manual/MCP usam a reconciliação comum, sem alterar
observações ou timestamps; ausência de âncora fica observável como
`no_session_page`. Retries/leases continuam limitados. Diagnóstico JSON/provider
contém classe/status, nunca o corpo da resposta.

Toolchain reproduzível, sem instalar pacotes globalmente:

```bash
docker build -t ai-memory-quality-rust:1.95 \
  -f local-maintenance-quality/Dockerfile.rust local-maintenance-quality
bash local-maintenance-quality/rust.sh fmt --all -- --check
git diff --check
bash local-maintenance-quality/rust.sh clippy --workspace --all-targets -- -D warnings
bash local-maintenance-quality/rust.sh --host-tests test --workspace --all-targets
bash local-maintenance-quality/rust.sh --host-tests test --workspace --doc
python3 -m unittest discover -s local-maintenance-quality/tests -v
```

`rust.sh` monta somente este worktree, seu git comum somente leitura e um cache
Cargo sob `target/quality-cargo`. Não monta HOME, config nem credenciais do
operador. A imagem deriva de Rust 1.95 com clippy/rustfmt dentro do Docker.
Sem nextest, `test --workspace --all-targets` é o full gate equivalente do repo.
O entrypoint cria uma conta/home descartável no container e executa Cargo com
uid/gid do operador. `--host-tests` remove somente os marcadores de container
da camada efêmera para exercitar a postura nativa dos testes de instalação e
permissões; não remove arquivos do host nem ignora testes/warns. Sem esse modo,
o teste nativo de diretório sem permissão recebe primeiro a recusa de container.

Evidência final: fmt/diff/clippy workspace `-D warnings` passaram; workspace
all-targets teve 4.337 testes aprovados e 14 ignorados existentes; workspace-doc
passou (sem exemplos executáveis). Python 13/13 e shell syntax passaram.
Build 1.95 e runner final tiveram exit 0: 27 formatos/controles, 11/11 MCP/FTS,
em `/tmp/ai-memory-quality-lab-1009-final-ttl/evidence.json`. Gitleaks local/diff
não encontrou segredos. `cargo deny`/nextest não estavam instalados.

Isso comprova a construção sintética; não substitui a identificação exata dos
cinco candidatos históricos para replay. A consulta autorizada de metadados
identificou dois em default/dev, já com resumo latest, e não encontrou os outros
três na base indicada pelo helper. O relato do dono registra essa pendência.

O dono integra conflitos com dream no outro worktree, faz backup/checkpoint,
deploy e eventual apply/replay pontual. O laboratório não avalia LLM real,
embeddings, respostas clínicas ou instalação efetiva dos CLIs dos fornecedores.
