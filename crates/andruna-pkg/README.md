# andruna-pkg

The package manager backend, used by andruna-sysd
([components](../../docs/spec/03-components.md#andruna-pkg-package-manager-backend),
[plan and status](../../docs/plan/phase2/pkg.md)). ADRs
[0027](../../docs/decisions/0027-installed-apps-entries.md) and
[0028](../../docs/decisions/0028-package-manager-in-rust.md).

| Module | What |
| --- | --- |
| `zip` | Zip reader with Android's checks (one end record reaching the end of the file, central directory adjacent to it, no zip64, stored/deflated only, local headers matching, no overlaps, unique names, CRC-32) |
| `axml` | Binary XML (`ResXMLTree`): string pools (UTF-8 and UTF-16), resource map, typed values |
| `arsc` | `resources.arsc`: dense, sparse and 16-bit entry tables, compact entries, configurations; lookup by locale (`iw`/`he` equivalent), density (`anydpi` first) and SDK |
| `manifest` | Package, versions (with `versionCodeMajor`), SDKs (codenames count as too new), application label and icon, activities and aliases with intent filters, permissions, `extractNativeLibs` |
| `sig` | Signature schemes v1 (JAR, PKCS #7), v2, v3 and v3.1 with proof-of-rotation lineage; stripping protection; update rules (same signer or a lineage ancestor with the installed-data capability) |
| `icon` | Launcher icon to a 256 px PNG and an `.ico` (16 to 256 px, PNG compressed): PNG/WebP bitmaps, vector drawables (paths, groups, clip paths, gradients), adaptive icons (inner 72 of 108 dp, rounded square mask), inset, layer list |
| `apk` | Parse and verify one APK; assemble a base and splits into one package; ABI and split selection |
| `store` | The per-user store: inspect, install (staged, swapped in by rename, rolled back on failure), update, uninstall |
| `db` | `system\packages.json`, atomic (write, flush, rename) and versioned |
| `compile` | dex2oat through `andruna-app --compile` |
| `desktop` | Start menu shortcuts with the AppUserModelID and icon; "Installed apps" entries |
| `hooks` | Install and uninstall hooks (sysd plugs the AppContainer profiles in here) |
| `sys` | Windows API wrappers (file mapping, random, shell links, registry); all `unsafe` code |
| `testkit` | Test-only builders (feature `testkit`): zip, binary XML, resource tables, APKs, signing with keys generated at test time |

## Tests

`cargo test -p andruna-pkg`:

- unit tests per module, including damaged-input loops (truncations and
  random byte flips must never panic);
- `tests/sig.rs`: every scheme with RSA 2048 and P-256 keys generated at
  test time, tampering in every zip section and the signing block, stripping
  protection, rotation, and cross-checks with the Android SDK's apksigner in
  both directions when it is installed;
- `tests/install.rs`: the store end to end (shortcuts read back through the
  shell, the uninstall entry under a test registry key);
- `tests/real_apks.rs`: `tests/fixtures/real/fixture.apk`, built by aapt2,
  d8, zipalign and apksigner (`build.py`, 13 KB, committed); Binary Eye from
  F-Droid (MIT), downloaded with a pinned SHA-256 (skipped offline unless
  `ANDRUNA_REQUIRE_NETWORK=1`); dex2oat for real when `ANDRUNA_APP_EXE` and
  `ANDRUNA_ART_ROOT` are set (required with `ANDRUNA_REQUIRE_GUEST=1`).
