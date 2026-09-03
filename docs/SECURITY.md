# Security model

Native PTY provider code is trusted application code and launches only the user-selected local program. Endpoint providers own transport credentials; the shared core exposes no SSH keys, agent handles, or authentication callbacks.

Third-party extensions are untrusted relative to the host. They receive only explicitly granted capabilities and filtered event streams. They cannot acquire endpoint transport authority through annotations, environment variables, composition, or lifecycle APIs.

Trusted Lua configuration is user-owned code and is intentionally trusted. The Lua VM includes standard libraries and is not a sandbox; never use this path for marketplace or third-party plugin code.

Extension queues have fixed event/output limits. Correctness-critical output is never silently discarded merely to keep a plugin alive; explicit backpressure lets host policy suspend or disconnect unhealthy extensions.

Shell annotations are optional metadata. Process lifecycle, byte transport, resize, and exit status do not depend on prompts, OSC annotations, command markers, or shell-specific integration.

The MVP contains no proprietary remote mux daemon, embedded browser, agent platform, cloud service, automatic credential discovery, or generic filesystem/network capability for third-party extensions.
