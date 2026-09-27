# Agent routing evaluation

`python3 scripts/eval-agent-routing.py` evaluates the deterministic pre-edit router against `routing-cases.json`. Cases cover high-risk execution tasks and separation from PTY, completion, parser, and other domains. Add a case when a routing mistake is found. Post-edit validation remains owned by `doctor validate`.
