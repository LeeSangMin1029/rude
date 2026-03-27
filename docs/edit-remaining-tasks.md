# 편집 기능 남은 작업

## 성능

### resolve suffix scan O(n) → O(log n)
- 위치: `crates/rude-intel/src/graph/build.rs:97-115`
- 현재: `name_index` 전체 linear scan (suffix 매칭)
- 개선: suffix별 reverse 인덱스 or 정렬된 suffix array + binary search

### expand_to_attrs 파일 I/O 중복
- 위치: `crates/rude/src/commands/edit/locate.rs:48-58`
- 현재: 매 locate_symbol 호출마다 파일 전체 읽기
- 개선: 같은 파일의 여러 심볼 locate 시 content 캐시

## 기능

### split이 관련 impl 블록 자동 포함
- 위치: `crates/rude/src/commands/edit/split.rs:7-81`
- 현재: struct만 이동, impl 블록은 원본에 남음
- 데이터: `trait_impls`, `impl_of_trait` 이미 그래프에 있음
- 구현: struct symbol → 그래프에서 관련 impl chunk indices 조회 → 자동으로 symbol_names에 추가

### multi-line use 문 지원
- 위치: `crates/rude/src/commands/edit/imports.rs:117-119`
- 현재: `is_use_line`이 한 줄(`;`로 끝나는)만 감지
- 데이터: mir_uses에 이미 정확한 per-item resolved path 있음
- 구현: cleanup_unused_imports와 filter_used_imports가 mir_uses DB 쿼리로 전환하면 multi-line 파싱 불필요

## 한계 (해결 불가)

### #[cfg] 플랫폼 조건부 코드
- `#[cfg(target_os = "windows")]` 등 — 현재 빌드 플랫폼 아이템만 MIR에 포함
- `#[cfg(test)]`는 이미 처리됨 (test target 분석)

### proc macro 내부 구현
- derive 매크로의 import는 매크로 크레이트가 관리 — 사용자 코드 import와 무관
- `bail!` 같은 function-like macro는 type alias chain resolve로 이미 해결
