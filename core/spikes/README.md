# Spikes

Throwaway programs that answer one question with a measurement instead of an
argument. Each is deleted once the decision it informed is recorded.

Spikes are excluded from the workspace and are not held to the workspace's lint
posture — they are instruments, not product code, and holding them to shipping
standards would make them expensive enough not to write.

| Spike | Question | Outcome |
|---|---|---|
| `handshake/` | Does hybrid post-quantum key agreement work over QUIC, and does it actually engage? | Yes — and a hybrid-preferred *list* downgrades silently, so exactly one group is offered. **Delete once the finding is a month old.** |
