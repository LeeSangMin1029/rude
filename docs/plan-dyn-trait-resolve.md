# dyn Trait 호출 resolve 계획 (2026-03-30)

## 문제

MIR callee `SourceDatabase::set_file_text` → chunk `<RootDatabase as SourceDatabase>::set_file_text` 매칭 실패.

## 수정 지점

`edge_resolve.rs`의 `resolve_callee` → `resolve_mir_name` 경로.

`resolve_mir_name`이 `name_to_idx`에서 못 찾을 때, callee 이름이 `Trait::method` 패턴이면:
1. `Trait` 이름으로 trait chunk 조회 → `trait_impls`로 impl chunk 목록 획득
2. 각 impl chunk에서 `::method` suffix가 일치하는 함수 chunk를 callee로 반환
3. 1:N 관계 (하나의 trait call → 여러 impl)이므로 `resolve_callee` 반환을 `Vec<u32>`로 변경

## 수정 파일

- `crates/rude-intel/src/graph/edge_resolve.rs` — resolve_callee, resolve_with_mir
- `crates/rude-intel/src/graph/build.rs` — trait_impls를 edge_resolve에 전달

## 검증

- rust-analyzer `RootDatabase` blast: affected > 0 확인
- rude 자체 테스트 122개 통과
