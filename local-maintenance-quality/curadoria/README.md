# Curadoria exata de relações

`helper.py` substitui o inventário histórico do draft de manutenção: seleciona
**apenas latest**, exige DB/URL/workspace/projeto explícitos e abre SQLite com
`mode=ro` + `query_only`. Nunca lê corpos ou títulos. Não altera SQL/wiki.
Requisições recusam redirects e mantêm o bearer apenas na URL explícita.

Dry-run padrão (root fornece token por stdin se necessário):

```sh
python3 local-maintenance-quality/curadoria/helper.py \
  --db /CAMINHO/EXPLICITO/memory.sqlite --server-url http://127.0.0.1:PORTA \
  --workspace default --project dev --out /ARTEFATO/PRIVADO/plan.json --token-stdin
```

`--apply --plan /ARTEFATO/PRIVADO/plan.json` exige o plano revisado e repete a
simulação HTTP de **todos** os candidatos antes da primeira mutação. O novo
`POST /admin/curate-relations` é root-only, no-create e recusa campos desconhecidos.
Exige `expected_head_id` e `expected_body_sha256`; o writer repete o CAS dentro da
transação. Cada alteração mantém a versão anterior e checkpoints antes/depois.
Body e abstract inalterados herdam seus embeddings na mesma transação, com
provider/model/dim preservados; a curadoria não chama um provedor de embeddings.
O TTL já indexado também é preservado quando faltar sua chave no frontmatter
de uma importação legada; a limpeza não renova nem remove uma expiração.

Somente `None`, `Nenhuma`, `N/A` e suas formas legadas `.md`, por comparação exata
ignorando caixa e espaço externo, são removidos dos arrays conhecidos de
`relations`. Não se removem `FALHA.md`, títulos, paths parecidos, origem, fatos
ou corpo. As ambiguidades ficam em relatório privado (0600), com valor original,
campo e origem. Nenhuma resolução LLM/Jev: esta operação decide igualdade exata
de valores, escopo, head e hash, sem julgamento semântico.
A origem usa os campos reais `session_id`/`agent`/`sources`; um `origin`
legado/customizado também permanece no relatório, sem consulta a observações.

Escritas autorizadas: `default/dev`, `default/farol`, `default/regente`.
QLMED retorna apenas contagem de páginas latest e sempre recusa apply; nem paths,
títulos ou frontmatter são selecionados nesse ramo. O root faz deploy/apply.

Uma alteração concorrente após o preflight cancela o candidato obsoleto e os
seguintes; candidatos anteriores já confirmados têm suas versões e checkpoints
recuperáveis. O helper não promete atomicidade entre requisições HTTP. Interrupção
ou erro de checkpoint requer conferir os receipts/checkpoints antes de retentar.
Admission que modifica qualquer valor protegido é recusada, mesmo em dry-run.
Páginas cujo corpo/frontmatter divergiram entre disco e índice são recusadas;
este helper não reindexa nem corrige conteúdo.
