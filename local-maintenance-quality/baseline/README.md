# Baseline sintético executável

`bench.py` foi copiado de `/home/marce/.agents/tools/brain/bench.py` e adaptado
somente neste laboratório. SHA-256 da fonte copiada:
`4bbe658daafb61b4f884a6c3be93bfd4b103b30c42c3dd4c8b907732036cbd7b`.
O original, `brain-sync.py`, os datasets e os logs globais não foram alterados.

A cópia conserva recall@3, MRR@10, IC95 de Wilson, latência, tamanho estimado de
resposta e comparação FTS5. `ESCOPOS` contém `regente` e `meusagentes`; as
consultas MCP e SQL incluem o workspace. Um path igual em outro workspace nunca
completa o gabarito. Só a versão latest não expirada conta.

`fixtures.json` contém 13 gravações para 11 páginas atuais e 11 perguntas com
fatos exclusivos e paths exatos. Há origem identificada, exceção depois de 6 KB,
decisão antiga substituída com histórico preservado, referência inválida
pendente, instrução maliciosa citada como dado e assuntos parecidos discriminados
por fatos diferentes. Não há placeholders a preencher nem resolução por título.

O runner grava tudo pelo `/admin/write-page`; os scripts nunca executam
INSERT/UPDATE/DELETE nem escrevem na wiki. O banco é aberto `mode=ro` e
`query_only=ON`. URL, DB, workspace e diretório devem coincidir com `lab.json`,
e `/admin/status` deve identificar aquele DB. O laboratório recusa a porta
padrão 49374, hosts externos, redirects e diretórios preexistentes.

Para o dono reproduzir somente o baseline com um binário explícito:

```bash
python3 local-maintenance-quality/run_lab.py \
  --binary /caminho/absoluto/ai-memory \
  --lab-dir /tmp/ai-memory-quality-baseline-novo \
  --server-url http://127.0.0.1:49481 \
  --workspace quality-synthetic \
  --baseline-only
```

Use diretórios novos para comparar baseline e piloto. `--baseline-only` também
permite um binário anterior ao endpoint de curadoria. O runner encerra seu
processo ao terminar e deixa `bench.jsonl`, `evidence.json` e o laboratório
privado para revisão. Ele gera config e root bearer exclusivamente sintéticos;
nenhuma credencial ou configuração do operador é herdada.

Para repetir a medição enquanto aquele servidor de laboratório estiver ativo:

```bash
python3 local-maintenance-quality/baseline/bench.py \
  --lab-dir /tmp/ai-memory-quality-baseline-novo \
  --server-url http://127.0.0.1:49481 \
  --db /tmp/ai-memory-quality-baseline-novo/data/db/memory.sqlite \
  --workspace quality-synthetic \
  --log /tmp/ai-memory-quality-baseline-novo/bench-repetido.jsonl
```

Evidência executada em 09/10: 11/11 recall@3 e MRR@10 = 1,0 tanto no MCP quanto
no FTS, sem erros, em `/tmp/ai-memory-quality-baseline-1009-01/evidence.json`.
O runner completo também passou o mesmo corpus em
`/tmp/ai-memory-quality-lab-1009-final-ttl/evidence.json`, após os gates finais.

Limites: isto mede recuperação de páginas com gabarito no corpo completo, como
o bench original. Não mede síntese de resposta, obediência de um LLM à injeção,
roteamento `ask.py`, recall vetorial ou desempenho clínico. O texto malicioso
permanece dado inerte; nenhum projeto indicado por ele foi criado. Nenhum LLM
foi executado. O dono fará o piloto com o artefato integrado e insumos autorizados.
