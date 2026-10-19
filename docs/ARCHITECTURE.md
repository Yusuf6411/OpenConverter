# Architecture

See MASTER_PROMPT.md §9 for the target design. Layering: oc-cli / oc-desktop / oc-server / oc-python -> oc-engine -> format crates -> oc-core / oc-io / oc-simd / oc-checksum / oc-compress. Format crates never depend on oc-engine or UI crates. This file is expanded as crates gain code.
