# Synthetic shift load testing

`shift.js` uses k6's per-VU-iterations executor: ten distinct clinicians, eighteen
patients each, eight encounters per patient, alternating one/two WAV chunks,
objective data, replayed event batches, event cursors and MDM receipts. It spreads
encounters across a compressed hour and polls every patient's cursor every ten
seconds while waiting. Default workload: 180 patients, 1,440 encounters and 2,160
chunks. Identity setup rejects reuse of a clinician across VUs.

## Run

Use a protected JSON credential file containing exactly one `{"token":"..."}`
object per VU, and a synthetic spoken WAV. Never use patient recordings. Install
[k6](https://grafana.com/docs/k6/latest/set-up/install-k6/) separately. This session
used official k6 v2.3.0 with its release checksum verified.

```bash
BASE_URL=http://127.0.0.1:8080 ACK_SYNTHETIC_LOAD=yes PROVIDERS=mock \
CREDENTIALS_FILE=/secure/load-credentials.json AUDIO_FILE=/secure/synthetic.wav \
k6 run load/shift.js
```

Mock runs require isolated local API/worker/storage configured with `APP_ENV=dev`,
no provider keys, and the existing mock fallback. The script rejects non-loopback
mock targets. The Step 8/9 cloud provider gates remain enforced: this implementation
does not introduce a staging flag that could silently emit mock clinical output.
Use production-like resources in a dedicated synthetic environment for measured
capacity work; local smoke numbers are not staging capacity evidence.

For the small real-provider run use `PROVIDERS=real CLINICIANS=1 PATIENTS=3`,
`BASE_URL` set to the accepted synthetic staging origin, and a spoken synthetic WAV.
All eight encounter cycles still run per patient. Record the image digest, resource
sizes, queue depths, time window, provider configuration and credentials' org IDs
in restricted run evidence. Tokens expire; provision them for the bounded run and
never put them in the summary or commit them. `SHIFT_SECONDS`, `ENCOUNTERS`,
`POLL_SECONDS`, `PATIENTS`, `CLINICIANS` can reduce a smoke run; altered profiles are
not full acceptance runs. Export a summary with `SUMMARY_FILE`.

The local integration gate runs the actual script against real disposable
Postgres/MinIO/SQS, an API listener and a mock worker:

```bash
K6_BIN=/absolute/path/to/k6 scripts/ci-services.sh
```

It uses one clinician, one patient, one encounter and a one-second compressed
shift, with fresh test credentials. Without `K6_BIN`, the explicit k6 integration
test is ignored; all other application tests still run. No clinical audio or
provider credentials are needed. Python reconciliation/schema checks:
`PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_step10.py`.

## Acceptance and current capacity conflict

API p95 must be under 300 ms (the job-status route and direct S3 transfer are
excluded); real MDM submission-to-observed-completion p95 must be under 30 seconds.
Errors, 429s and changed acknowledgments fail the run. The completion counter must
match all intended patients. MDM submissions are paced thirteen seconds apart to
respect the separate five/minute quota.

**The default profile currently conflicts with the Step 9 60/minute clinician
quota.** Eighteen cursors every ten seconds require 108 reads/minute before any
writes or audio/status requests. This is an open acceptance failure, not evidence
of a server latency defect. The harness reports 429 and fails rather than reducing
polling or bypassing quotas. Resolve through a reviewed quota/capacity decision or
a multiplexed sync API before claiming the planned full-profile gate passed.
There is no hidden load-test quota bypass.

After every run, drain and record both visible and in-flight main/DLQ counts using
the scoped queue operator role, run `load/verify.sql` and `docs/ops/cogs.sql` for the
exact org/time window, and confirm all expected jobs/chunks completed. k6 alone
cannot certify DLQ emptiness or database uniqueness. API receipt replay checks and
the database query complement one another. Retain only aggregate summaries outside
restricted storage; default k6 metric tags omit raw URLs, tokens and presigned URLs.
Do not enable HTTP debug or request-body output on an authenticated run.

## Costs

Set the worker's `DEEPGRAM_MICROS_PER_MINUTE` to the reviewed contracted rate
before the real run; in Terraform release roots set the validated
`deepgram_micros_per_minute` input and deploy replacement worker tasks (e.g. 9000 means $0.009/minute; that is an illustration, not a
current price assertion). Missing, invalid or zero pricing produces an explicitly
`unpriced` STT row. Successful STT duration and frozen cost are recorded once per
chunk. Mock rows remain labelled mock/unpriced. Crash-after-provider-call costs
can exceed durable receipts; provider invoices are the reconciliation authority.
No ledger can infer calls lost before persistence.

Compare Bedrock and Deepgram independently for the same isolated run/window, net
of unrelated account traffic and with matching currency/discount treatment. Cost
Explorer aggregates infrastructure separately; activate the existing case-sensitive
`Project` and `Env` cost allocation tags, including shared bootstrap resources.
These tags already exist on all five provider roots. They are not lowercase
`project`/`env`. Untaggable resources and shared-account services need allocation.

`python3 scripts/reconcile-costs.py evidence.json` accepts each provider's
`ledger_micros` and `invoice_micros`, fails on unavailable/zero invoice evidence,
and requires relative absolute error at most 5% for **both** providers. Console
estimates before billing settles are provisional. A suggested evidence shape is
in that script's module documentation; save original supporting exports separately.
The COGS query attributes missing clinician IDs to patient creator, which is an
allocation convention rather than proof of who recorded a shared-care encounter.
Include unmatched rows, STT, model cache reads/writes, infrastructure allocation,
idle staging and provider minimums when assessing margins. The historical
$510–700 pilot band is a planning assumption, not a measured or current quote.

Full-hour capacity, the real-provider mini-run, invoice comparison and monthly
run-rate evidence remain pending live acceptance. No cloud spend was initiated by
this implementation.
