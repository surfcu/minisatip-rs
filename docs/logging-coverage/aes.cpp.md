# aes.cpp logging coverage

Ported 0 / not applicable 8 (8 log sites).

`aes.cpp` is an OpenSSL EVP wrapper (`DEFAULT_LOG` is `LOG_PMT`). The Rust
replacement is a pure FIPS-197 AES implementation in
`crates/ms-ca-drivers/src/descramble/`; there is no cipher context allocation,
no `EVP_*` return code, no fixed 400-byte scratch buffer and no CBC fallback
path, so none of these failure branches exist. With no ported line there is no
exact-text logging test for this file.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 54 | LOG | EVP_CIPHER_CTX_new failed | n/a: pure-Rust AES has no context allocation that can fail |
| 70 | LOG | EVP_DecryptInit_ex failed for ECB mode | n/a: no EVP init/return code; the key schedule is infallible |
| 85 | LOG | EVP_DecryptUpdate failed | n/a: no EVP update/return code; block decryption is infallible |
| 99 | LOG | EVP_CIPHER_CTX_new failed | n/a: same as line 54 (CBC context) |
| 117 | LOG_AND_RETURN | AES CBC context is null | n/a: no opaque context pointer exists |
| 120 | LOG_AND_RETURN | EVP_DecryptInit_ex failed | n/a: no EVP init/return code |
| 124 | LOG_AND_RETURN | EVP_DecryptUpdate failed | n/a: no EVP update/return code |
| 140 | LOG | WARNING: requested AES decryption of a large payload with  | n/a: the Rust decrypts in place with no fixed 400-byte scratch buffer, so payloads are never truncated |
