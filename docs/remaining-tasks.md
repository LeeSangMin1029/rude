# Remaining Tasks (2026-03-27)

## 1. syn 기반 locate_symbol — 편집 정확성 근본 해결
**현재 문제**: `locate_symbol`이 MIR chunks 캐시 라인 번호에 의존.
파일 편집 후 라인이 밀리면 다음 편집이 틀린 위치를 잡음.
`{}` 깊이 추적이 문자열/주석 구분 못 함.

**해결**: syn으로 파일 직접 파싱 → 정확한 심볼 위치.
- rude CLI에 syn 의존성 추가
- locate_symbol: syn::parse_file → Item::Fn/Struct/Enum의 span
- MIR chunks는 분석(call graph)에만 사용, 편집에는 사용 안 함
- 편집 후 re-scan 불필요 (매번 파일에서 직접 파싱)
- 예상 비용: ~50ms/파일 (syn 파싱)

## 2. db 전역화 리팩토링 — 코드 일관성
**현재 문제**: `db: &Path`가 20+ 함수에 반복 전달.
`StorageEngine::open(db)` 15회 반복. `db.parent()` 8회 반복.

**해결**: rude-intel에 ctx 싱글톤 모듈.
- ctx::init(db_path) → set once
- ctx::db(), ctx::project_root(), ctx::mir_db() → 어디서든 접근
- loader.rs, build.rs, edge_resolve.rs 함수 시그니처에서 db 제거
- Task 1 완료 후 rude replace로 리팩토링 (연속 편집이 안전해지므로)

## 3. nightly target-dir 격리 검증
**현재**: cli.rs에 target/mir-check-{nightly_hash} 적용됨.
**남은 것**: rust-analyzer 같은 대형 프로젝트에서 stable↔nightly 충돌 없이
증분 동작하는지 검증.

## 의존 관계
1 → 2 → 3 (순차)
1이 완료되면 2를 rude replace로 안전하게 리팩토링 가능.
