# Fixture provenance

- `cx/*.spec.json` — minimal regression specs from the tblgen review corpus
  (`/tmp/tblgen-review/cx/`): RemoveField case-folding, Refactor rename
  collision, spec-key vs alias join qualifier, spec key/wire id collision,
  raw default Output partition.
- `cx/*.tbl` — the corresponding outputs emitted by the PREVIOUS generator
  (same `/tmp/tblgen-review/cx/` corpus); each carries the defect its spec
  exercises, so `check` must report the targeted diagnostic.
- `experiment.spec.json` — a full DataFlow model spec (`/tmp/tblgen-rebuild/model.spec.json`):
  6 ModelTable sources, Select/Join/Union/Distinct chains, AddField/Refactor/
  TransformToCalculatedField steps, explicit Union `unionMapArray`.
- `ads_owner_repair_history.tbl` — the original experiment output from the
  previous generator (`/tmp/tblgen-rebuild/`), not a server-saved fixture.
  `check` reports its legacy Union map key and AddField operand errors.
- `dataflow_sql.tbl`, `dataflow_pres.tbl` — real server-saved `.tbl` files
  copied verbatim (unmodified) from the platform test corpus:
  `com.succez.bi/src/test/metadata/TestCase/data/tables/dataflow/BASIC/dataflow_sql.tbl`
  and `.../tables/query/dataflow_pres.tbl`. They carry `lastModifyTime`
  values and non-closed field keys, so they exercise the archive fallback.
