# Development

Build from this project directory with PowerShell 7, Rust MSVC and LLVM's
`lld-link` on PATH:

```powershell
./build.ps1
```

The output is `dist/hbr-account-export-v0.1.0-win-x64.zip`. Build runs formatting,
source hygiene, locked Cargo tests, release compilation, the 16 version exports
and real Windows version-resource read/query, dependency notices and package
path scanning. A disposable native smoke host checks DLL thread notifications
and a first version API call on a 64 KiB worker stack. No .NET SDK or generated
interop is required.

## Runtime

The DLL forwards Windows version APIs through the OS system directory. DllMain
uses stack-only executable/CEF process gating and dispatches a worker outside the
loader lock. The worker first polls only `il2cpp_get_corlib`, then observes a stable
assembly inventory without calling the lazy domain getter. It resolves
all required metadata, then installs process-lifetime detours.

Version 0.1.0 called `il2cpp_domain_get` before checking corlib. Two native crash
dumps identified the same null-class access at GameAssembly offset `0x5CB507`,
through the lazy domain allocator (`0x57F6E0`), before any hook installation.
The verified `il2cpp_get_corlib` implementation reads a global pointer, while
`il2cpp_domain_get_assemblies` reads the assembly vector and ignores its domain
argument. The startup probe uses those reads only, with bounds checks and a
stable-inventory gate before thread attachment. A regression exercises the cold
runtime call ordering; it failed before the fix and passes afterward.

The first real-game retry passed runtime discovery and installed the hooks, then
faulted in the proxy's stack probe with `0xC00000FD`. DllMain reserved `0x10068`
bytes before testing its reason argument, and static TLS can prevent disabling
thread notifications. A native 64 KiB worker reproduced that overflow. The
entry now uses a small gating buffer, and the system version path buffer lives
on the heap. The same native host caught a separate overflow in the first
version API call after the entry fix; both paths are part of release checks.

After those fixes, live startup reached successful login and the first export
update. The next fault was in `il2cpp_gchandle_get_target` at GameAssembly offset
`0x5B7210`: this Unity runtime uses pointer-width GC handle slab addresses, not
32-bit indices. The binding, stored Root handle, get_target and free now use
`usize`. The ABI regression roundtrips a handle above 4 GiB through native
function pointers and verifies the value passed to free; it failed under the
old declarations and passes with the corrected binding.

Live method/type diagnostics then identified `get_Item (KeyNotFoundException)`.
Native `UWRServerRequest.PrepareHeaders` merges request/base headers and the retry
count; it does not yet contain `x-player-id`. That key is added later by
`SignatureProvider.GenerateHeaderCollection`. The route gate remains at the
first funnel, while signed UID validation now observes the actual generated
collection. Responses require an observed matching signed identity. Diagnostic
errors include only method and exception type, never exception messages or
request bodies.

The success callback for `GGLSequences.Prologue.GGLQueryXUID_Init` observes the
server-returned game UID. A saved PlayerPrefs UID alone cannot arm the exporter.
The provider's current UID and each export request's `x-player-id` must agree.
Native login, account-transfer and agreement handling remains in the game.
With a saved numeric UID, `TapToStartState.OnStateBegin` arms the game's existing
`Lily.Prologue.skip_tap` path and `GGLSequences.Prologue.WaitTapToStart_Proc` uses
the native `TapToStart` method to continue into authorization. The latter wait
occurs before server login. A saved UID permits only automating this tap;
export requests still require this launch's successful login callback.

`DeltaCommController.Update` is fenced to the selected controller and read phases.
The original method inlines `LoadFirstPacket`; a detour on that method would miss
the persisted push path. For one Update call, the exporter supplies an in-memory
single-packet queue and scopes the selected DB's `GetUShort("id0"/"id1")` reads to
zero/zero. The original queue reference is restored by a drop guard, including on
Rust panic. The DB and pending packets are retained. Other DB reads are forwarded.
The exporter supplies only `confirm` or `pull` packets and retains the game's
headers, transport, signing and signature verification. Normal DeltaComm updates
are suppressed during this one-shot export, including while waiting for login.

The confirm/pull success callbacks are consumed directly as raw JSON, before the
normal callbacks load data into the model or commit to the local save. The
initial confirm determines the complete table set; reads use batches of at most
15 tables and continue any server-size-limited partial response until its batch
is exhausted. Unknown/empty tables are retained. Duplicate, foreign, missing or
malformed data cannot produce a successful archive. JSON numbers retain their
lexical precision.

After pulling all tables, `GreeGameLib.Payment.queryBalanceAsync(-1)` supplies
free/paid/total gems. A second confirm must exactly match every pulled hash and
the table set. Drift restarts the entire read, with three attempts maximum. This
certifies the observed table versions; the protocol does not provide an atomic
transaction covering the game tables and separate payment service together.
Gameplay and login-bonus actions after the login snapshot are outside this export.

The archive follows `lilypad-export/1`: account, tables, officialHashes, extras and
stats. An exclusive temporary file is flushed and read back before rename.
Existing files are retained. Output ancestors reject links/reparse points.
A hidden Windows PowerShell process waits for the game process to exit and shows
the completion dialog. Only a committed archive and a ready notifier authorize
process exit; failure keeps the game open. ExitProcess avoids running an ordinary
Unity shutdown sync after the read. Export timeouts and failures stop export work
and require a fresh launch; no partial JSON is published as success.

## Native compatibility evidence

The implementation was checked against the supplied Windows x64 game metadata,
native database and decoded startup traffic. Evidence locations below are VAs
for investigation, not runtime addresses or required build inputs:

| Native function | Observed behavior | VA |
| --- | --- | --- |
| DeltaCommController.Update | Inlined queue selection; separate confirm/pull callbacks | `0x181664170` |
| GGLSequences.Prologue success closure | Server userId converted to string, SetXUID, next login state | `0x183939860` |
| Lily.SignatureProvider.XUID / SetXUID | UID is persisted in PlayerPrefs; cache alone is insufficient | `0x18115E930` / `0x18115E870` |
| TapToStartState.OnStateBegin | Native skip_tap transition | `0x18137DF80` |
| GGLSequences.Prologue.WaitTapToStart_Proc / TapToStart | Login tap precedes authorization; native state-12 gate | `0x1839387E0` / `0x183937B40` |
| DeltaCommDB.GetUShort | Interface queue-index reader, no write side effects | `0x1816BC030` |
| GreeGameLib.Balance.init | Separate free, charged and total balances | `0x1859619B0` |

Required assemblies are DeltaComm, HTTPComm, HTTPCommGenerated, GGLSequences,
Assembly-CSharp and WFS.Gamelib.Payment. Compiler-generated callback names are a
compatibility requirement; missing metadata is a visible failure. By-name lookup
does not establish universal compatibility after a game update. The x64 reference
array header uses data offset 0x20. Preserve ABI validation when updating bindings.

For an optional offline protocol/format regression, supply a decoded JSONL capture
and an output directory. Both remain private local files:

```powershell
$env:HBR_EXPORT_CAPTURE = '<decoded JSONL>'
$env:HBR_EXPORT_TEST_OUTPUT = '<temporary output directory>'
cargo test --locked --target x86_64-pc-windows-msvc --test capture -- --ignored --nocapture
```

This test uses captured confirm and pull hashes, not a live final confirm, and
does not execute injection or validate game shutdown. The JSON can be checked
with a consuming Lilypad import validator in a separate checkout; that consumer
is not a dependency of this project.

## Live validation

On 2026-10-07, it was exercised through Steam against the supplied
Windows x64 client (GameAssembly PE timestamp `0x6AAC3375`). The existing login
completed automatically, 232 confirmed tables were read in 22 response chunks,
and the 22,074-row archive included the payment balance. The final confirm and
file readback passed; the game exited automatically and the user confirmed the
completion dialog appeared. The generated archive was also accepted by the
current Lilypad parser, including the balance validator. No production import
was performed. Captures, account identity and the generated JSON remain local,
untracked runtime data.

This establishes the tested game/account flow; compiler-generated metadata names
and native ABIs still require revalidation for other builds and game updates.
