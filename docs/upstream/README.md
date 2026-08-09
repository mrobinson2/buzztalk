# Upstream contributions to block/buzz

BuzzTalk's real delivery vehicle is `block/buzz`, not a release page: users get voice when
it ships inside Buzz. These are the changes worth proposing upstream, each written to stand
on its own merits so it can be reviewed without anyone caring about BuzzTalk.

Ordered by how easy they are to accept.

| # | change | size | independent justification |
|---|---|---|---|
| 1 | agent-voice hash seed | 1 line | agents currently get a different voice every huddle |
| 2 | ~~sherpa-onnx 1.12 → 1.13~~ | — | **WITHDRAWN** — Buzz already resolves 1.13.4; the bump changes nothing |
| 3 | `AudioSource` / `AudioSink` seam | ~200 lines | makes huddle audio testable without a webview |

Nothing here mentions BuzzTalk as a reason. If a change cannot be justified to the Buzz
maintainers on Buzz's own terms, it does not belong upstream.
