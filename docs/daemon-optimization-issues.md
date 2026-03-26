# Daemon Pipeline Issues & Solutions (2026-03-27)

## Issue 1: Nightly 버전 충돌
stable 빌드 캐시(.rmeta)가 target/mir-check에 남아있으면 nightly rustc_public과 ABI 충돌.
**해결**: `target/mir-check-{nightly_hash}` — Miri 동일 전략.
**참고**: cargo build-dir v2 (2026-03-13), miri#1311

## Issue 2: lib rustc-args 누락
Issue 1이 근본 원인. stable 캐시 존재 → cargo가 lib skip → wrapper 미호출 → .lib.rustc-args 미생성.
test fallback은 cfg(test) 차이로 그래프 왜곡. Issue 1 해결하면 자연 해결.

## Issue 3: rustc 300ms (incremental 미사용)
rustc_public::run!이 매번 full pipeline. -C incremental 미사용.
**해결**: daemon process()에서 args에 `-C incremental=<path>` 주입.
**기대**: 300ms → 100ms 이하.
**참고**: rustc-dev-guide/incremental-compilation, RFC 1298

## Issue 4: Supervisor relay overhead ~100ms
Client→pipe→supervisor→stdin→worker→stdout→supervisor→pipe→client.
**해결**: Worker가 Named Pipe 직접 서빙, supervisor 제거.
**참고**: interprocess crate, catch_unwind로 crash 보호
