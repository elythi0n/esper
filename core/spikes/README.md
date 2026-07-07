# Spikes

Throwaway programs that answer one question with a measurement instead of an
argument. Each is deleted once the decision it informed is recorded.

Spikes are excluded from the workspace and are not held to the workspace's lint
posture — they are instruments, not product code, and holding them to shipping
standards would make them expensive enough not to write.

| Spike | Question | Outcome |
|---|---|---|
| `rpk/` | Can two peers authenticate by bare Ed25519 public key over QUIC, with no CA? | Yes. Two traps found: the ordinary TLS 1.3 signature verifier parses an X.509 and must be swapped for the raw-key variant, and a completed `connect()` does not mean the peer accepted you. **Delete once folded into ownview-transport.** |
| `handshake/` | Does hybrid post-quantum key agreement work over QUIC, and does it actually engage? | Yes — and a hybrid-preferred *list* downgrades silently, so exactly one group is offered. **Delete once the finding is a month old.** |
