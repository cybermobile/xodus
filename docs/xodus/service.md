# xodus-service IPC

`xodus-service` is the long-running daemon that game-side code (the
`xgameruntime.dll` implementation running under wine) talks to for
authentication and, over time, the rest of the Xbox services surface. It
listens on a Unix socket:

- **Linux:** `$XDG_RUNTIME_DIR/xodus.sock`
- **macOS:** `/tmp/xodus.sock`

Both ends resolve this path through `xodus::ipc::socket_path()` so they can
never disagree. The socket is created with mode `0600`, and connections are
logged with the peer PID (`SO_PEERCRED`).

`xodus-cli run` takes care of the daemon automatically: it connects to a
running service (verified with a ping) or spawns one (looking for
`xodus-service` next to the `xodus-cli` binary, then in `PATH`), and passes
the endpoint to wine in the **`XODUS_SOCKET`** environment variable — that is
the contract for the wine-side runtime. A launch without a working service
still proceeds, with a warning; Xbox Live services are just unavailable
in-game. If `run` spawned the service itself, it stops it again (SIGINT,
then kill) once the game exits.

## Framing

Every message, in both directions, is one frame (all integers little-endian):

| field | type | meaning |
| --- | --- | --- |
| magic | `u32` | transport: `0x58445358` ("XSDX", XML) or `0x58445350` ("PSDX", protobuf — reserved, not implemented) |
| type | `u16` | `XodusMessageType` (`crates/xodus/proto/xodus/common.proto`) |
| size | `u16` | payload byte length (so payloads are capped at 64 KiB) |
| payload | bytes | XML-serialized message body |

A request's success response uses the request's message type + 1 (`Ping` →
`Pong`, `MsaTokenRequest` → `MsaTokenResponse`) and echoes or answers the
payload. A failed request answers with **`ErrorResponse`** (type 5) instead,
carrying `<ErrorResponse><Message>…</Message></ErrorResponse>` — clients must
check the response type before parsing the body. Malformed frames close the
connection.

## Message types

| type | payload | notes |
| --- | --- | --- |
| `Ping` (1) | arbitrary bytes | echoed back as `Pong` (2); used as the liveness probe |
| `MsaTokenRequest` (3) | `<MSATokenRequest><ClientId>…</ClientId></MSATokenRequest>` (optional `AllowUI`, `MSAFullTrust` flags) | brokers an MSA user token for the given client id from the stored login; replies `MsaTokenResponse` (4) with `Token`, `Expiry`, `DeviceRps`, `DeviceExpiry` |
| `UserIdentityRequest` (6) | empty | resolves an XSTS token for the signed-in user and replies `UserIdentityResponse` (7) with `Xuid`, `Gamertag`, `ModernGamertag`, `UserHash`, `Expiry`; feeds the wine-side `XUser` surface |

Requests the service does not recognize get an `ErrorResponse`. A request
that requires a logged-in user (such as `MsaTokenRequest` without a stored
STS token) reports that in the error message — run `xodus-cli login` first.

`scripts/send_msa_token_request.py` is a minimal reference client.
