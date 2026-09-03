# Architecture

## Small core

`terminal-core` owns identities, lifecycle contracts, surface binding, declarative layout, command metadata, optional annotations, compatibility diagnostics, transition measurements, and bounded extension delivery. It intentionally does not own rendering, SSH authentication, a remote session daemon, tmux protocol logic, shell parsing, or filesystem/network plugin APIs.

The public model is `Endpoint + Surface + Command + Annotation`. Endpoint capabilities are explicit and unequal by design; surface rebinding does not imply persistence; commands are application actions separate from byte streams; annotations are optional metadata and cannot affect transport correctness.

## Endpoint providers

`local-pty` is the only concrete provider in the MVP. It advertises no persistence and no detachment. SSH and tmux are compatibility assumptions rather than hidden core behaviors. A future SSH provider owns transport/authentication; a future tmux provider must explicitly advertise detachment before persistent-session semantics are shown. `TERM`, `TMUX`, and `SSH_CONNECTION` are advisory observations, not authority.

## Composition and switching

Composition is a declarative split tree over known surfaces. Transition tracking records focus/rebind latency using caller-supplied monotonic timestamps without embedding UX thresholds.

## Extension boundary

Third-party extensions request explicit capabilities. Events are filtered before bounded inbox delivery. Adjacent output may coalesce, advisory events may be dropped explicitly, and correctness-critical output reports backpressure. Host policy may suspend or disconnect repeatedly backpressured extensions.

Trusted user Lua config is a separate, intentionally privileged origin and is never used as a loader for third-party extensions.

## No proprietary remote mux

Remote persistence is left to endpoint/provider layers such as tmux rather than reimplemented as a daemon in core.
