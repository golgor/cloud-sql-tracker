# Research brief — Config Byte Limits and Output Document Caps

Snapshot date: 2026-08-24
Primary links:
- [RFC 8259 — The JavaScript Object Notation (JSON) Data Interchange Format](https://datatracker.ietf.org/doc/html/rfc8259)
- [JSON Schema Draft 2020-12](https://json-schema.org/draft/2020-12/schema)
- [The Unicode Standard — UTF-8 Encoding](https://www.unicode.org/versions/latest/)

---

## Executive Summary

**Pick:** Enforce printable ASCII config strings and hard byte caps on Status (256 KiB) and Doctor (64 KiB) JSON output.
**Why:** Guarantees well-defined maximum output sizes to protect control plane consumers and UI bars.
**Discarded:** Allowing arbitrary UTF-8 in config strings or relying only on in-process serde without final output size checks.
**Unchanged:** Existing `id` character rules and the JSON Schema version 1 remain unchanged.

---

## String Representation and Byte Distinctions

This implementation distinguishes four different representations of string length:

1. **Decoded UTF-8 bytes:** The raw decoded string in Rust memory. Printable ASCII characters (`0x20` through `0x7E`) use exactly 1 byte per character.
2. **JSON source escapes:** Escape sequences in JSON source files. For example, `\n` uses 2 source bytes to represent 1 decoded newline byte.
3. **JSON output escaping:** Escape sequences created when serializing Rust strings to JSON. Quotes (`"`) and backslashes (`\`) expand 2x (`\"` and `\\`). Control characters (`0x00` through `0x1F`) expand up to 6x (`\u00xx`).
4. **JSON Schema character limits:** Because config strings require printable ASCII (`0x20` through `0x7E`), 1 character equals 1 UTF-8 byte. JSON Schema `maxLength` constraints exactly match Rust byte limits.

---

## Config String Limits and ASCII Policy

All config string fields require printable ASCII bytes `0x20` through `0x7E`.

| Field | Max Bytes (Decoded UTF-8) | Max JSON Output Bytes | ASCII Rule |
|-------|--------------------------|-----------------------|------------|
| `id` | 64 | 64 | `^[a-zA-Z0-9][a-zA-Z0-9_-]*$` |
| `name` | 64 | 128 (2x) | Printable ASCII (`0x20`–`0x7E`) |
| `group` | 32 | 64 (2x) | Printable ASCII (`0x20`–`0x7E`), no leading `-` |
| `instance` | 256 | 512 (2x) | Printable ASCII (`0x20`–`0x7E`), `project:region:instance` |
| `address` | 253 | 506 (2x) | Printable ASCII (`0x20`–`0x7E`) |
| `proxy_bin` | 4095 | 8190 (2x) | Printable ASCII (`0x20`–`0x7E`) |
| `extra_args` | 16 items / 2048 total | 4096 (2x) | Printable ASCII (`0x20`–`0x7E`) per element |

---

## Output Document Analysis and Upper Bounds

### Status Document

- **Connections limit:** Maximum 32 rows (`connections.json`).
- **Groups limit:** Maximum 32 groups.
- **Error detail limit:** Clamped to at most 512 raw UTF-8 bytes at production seams. External error messages can contain control characters (`0x01`), expanding 6x (3072 bytes) in JSON output.
- **Field-wise conservative hard upper-bound fixture:** 160,586 bytes when serialized with `serde_json::to_string_pretty` plus a trailing newline. Every row carries maximum length `id` (64 B), `name` (64 quote/backslash B -> 128 B JSON), `group` (32 B -> 61 B JSON), `instance` (256 B -> 498 B JSON), `address` (253 B -> 508 B JSON), `unit` (88 B -> 90 B JSON), `pid` (`u32::MAX`), `uptime_sec` (`u64::MAX`), `cli_version` (64 B), and a 512-byte control-character `error.detail` (3072 B JSON).
- **Observed 32-row max config input binary fixture size:** 52,965 bytes for 32 maximum-size connections with quotes and backslashes in config.
- **Enforced cap:** 256 KiB (262,144 bytes).

#### Status Document Serialization Formula

| Field / Component | Raw Input Bounds | JSON Escaped Bytes | 32-Row Subtotal |
|-------------------|------------------|--------------------|-----------------|
| Top-level keys / ts / version / aggregates | Fixed fields | ~200 B | ~200 B |
| `cli_version` | 64 B printable ASCII | 66 B (with quotes) | 66 B |
| `groups` map (32 entries) | 32 B key + GroupCounts | ~140 B per entry | ~4,480 B |
| `connections[].id` | 64 B printable ASCII | 66 B | 2,112 B |
| `connections[].name` | 64 B quote/backslash | 130 B | 4,160 B |
| `connections[].group` | 32 B quote/backslash | 63 B | 2,016 B |
| `connections[].instance` | 256 B 3-part quote/backslash | 498 B | 15,936 B |
| `connections[].address` | 253 B quote/backslash | 508 B | 16,256 B |
| `connections[].unit` | 88 B unit name | 90 B | 2,880 B |
| `connections[].pid` / `uptime_sec` / `port` / flags | Max integers / bools | ~65 B | ~2,080 B |
| `connections[].error.detail` | 512 B control chars (`\x01`) | 3,074 B | 98,368 B |
| JSON keys, commas, spaces, newlines | Formatting overhead | ~380 B per row | ~12,160 B |
| **Total Status JSON (with `\n`)** | | | **160,586 B** |

### Doctor Document

- **Checks limit:** Fixed at 6 checks (`config`, `proxy_bin`, `systemd_user`, `adc`, `journal_user`, `ports`).
- **Detail and hint limit:** Each `detail` and `hint` string is clamped to at most 512 raw UTF-8 bytes at production seams (up to 3072 bytes in JSON output when escaped with `0x01` control characters).
- **Field-wise conservative hard upper-bound fixture:** 37,588 bytes when serialized with `serde_json::to_string_pretty` plus a trailing newline. All 6 checks carry 512 control-character bytes in both `detail` and `hint`, with a 64-byte `cli_version`.
- **Observed binary Doctor fixture size:** 1,440 bytes for a full 6-check report with dynamic text.
- **Enforced cap:** 64 KiB (65,536 bytes).

#### Doctor Document Serialization Formula

| Field / Component | Raw Input Bounds | JSON Escaped Bytes | 6-Check Subtotal |
|-------------------|------------------|--------------------|------------------|
| Top-level keys / version / ok | Fixed fields | ~50 B | ~50 B |
| `cli_version` | 64 B printable ASCII | 66 B (with quotes) | 66 B |
| `checks[].id` + `status` | Fixed check IDs | ~30 B per check | ~180 B |
| `checks[].detail` | 512 B control chars (`\x01`) | 3,074 B | 18,444 B |
| `checks[].hint` | 512 B control chars (`\x01`) | 3,074 B | 18,444 B |
| JSON keys, commas, spaces, newlines | Formatting overhead | ~67 B per check | ~404 B |
| **Total Doctor JSON (with `\n`)** | | | **37,588 B** |

---

## Output Size Protection (Backstop Invariant)

Immediately before writing to stdout for `status --json` and `doctor --json`:

1. The report serializes to a pretty JSON string in memory with a final trailing newline (`\n`).
2. The total emitted byte length (including the final trailing newline) is compared against the command cap.
3. If the length is within the cap, the document is printed to stdout with its final trailing newline.
4. If the length exceeds the cap, **no JSON** is written to stdout, an error message is printed to stderr, and the process exits with code **3**.
