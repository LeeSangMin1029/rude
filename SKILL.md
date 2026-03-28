---
name: rude
description: "코드 심볼 분석+편집. 정의/참조, 호출 그래프(MIR 기반 100% 정확), blast radius, 경로 추적, 중복 감지, dead code, 실행 기반 커버리지, 심볼 기반 편집, 자동 모듈 분리. .code.db 필요."
user-invocable: true
---

# rude — 코드 구조 분석 + 편집 도구

DB: `.code.db` | 옵션 불확실 시 `rude <cmd> --help`로 확인

## 커맨드 요약

| 커맨드 | 용도 |
|--------|------|
| `context` (ctx) | 통합 컨텍스트: 정의+caller+callee+타입+테스트 |
| `trace` (tr) | 두 심볼 간 최단 호출 경로 |
| `dead` | caller 없는 함수 (unreachable) |
| `dupes` (dup) | 중복 코드 탐지 |
| `coverage` (cov) | 테스트 커버리지 (llvm-cov) |
| `symbols` | 심볼 검색 |
| `stats` | 크레이트별 통계 |
| `aliases` | 경로 별칭 매핑 ([A], [B] 등) |
| `cluster` | 파일 내 독립 함수 클러스터 분석 |
| `add` | MIR 기반 인덱싱 (증분). DB 없으면 자동 생성 |
| `watch` | 파일 변경 자동 감시 + 증분 업데이트 |

## 편집 커맨드

| 커맨드 | 용도 |
|--------|------|
| `replace` (rep) | 심볼 본체 교체 |
| `insert-after` / `insert-before` | 심볼 앞뒤 삽입 |
| `delete-symbol` (del) | 심볼 삭제 |
| `insert-at` (ia) | 특정 라인에 삽입 |
| `replace-lines` (rl) / `delete-lines` (dl) | 라인 범위 교체/삭제 |
| `create-file` (cf) | 새 파일 생성 |
| `split` | 심볼을 새 모듈 파일로 분리 |
| `split-module` (sm) | 파일→디렉토리 모듈 변환 + 심볼 자동 분배 |
| `clean-imports` (ci) | 미사용 import 제거 |
| `ensure-import` (ei) | import 추가/병합 |

## 모듈 분리 (`split-module`)

**수동 지정**:
```
rude split-module --file watch.rs "watcher.rs:run" "handler.rs:process_changes,update_db"
```

**자동 분리** (call graph 기반):
```
rude split-module --file watch.rs --auto
```

동작:
- 파일→디렉토리 변환 (foo.rs → foo/mod.rs)
- 진입점(pub fn) 기준 그룹핑, 공유 유틸은 mod.rs 잔류
- `super::` → `crate::` 경로 자동 변환
- visibility 자동 조정 (private fn → pub(super))
- cross-module import 자동 생성
- unused import 자동 정리

## 설정 (`.code.db/config.toml`)

```toml
[split]
min_lines = 300    # 자동 분리 대상 파일 최소 줄 수

[cluster]
min_lines = 50     # 별도 파일로 분리할 그룹 최소 줄 수
```

우선순위: CLI args > config.toml > 기본값

## 필수 규칙

- **코드를 읽을 때 `rude context -s`를 우선 사용** — Read로 수백줄 읽지 말 것
- rude가 알려준 **라인 범위**로 `Read(offset, limit)` 범위 읽기
- 편집 시 **heredoc stdin** 권장 (`cat <<'EOF' | rude replace ...`)
- `cargo run -p rude` 금지 — PATH의 `rude` 직접 사용
- replace/split 결과가 stdout에 출력되므로 **확인용 Read 불필요**

## 동시성

- **읽기**: 동시 실행 안전
- **편집**: `.lock` exclusive lock — 동시 편집 안전
- **DB 쓰기** (add): 동시 실행 피할 것

## 제약

- **nightly rustc 필요** — `mir-callgraph`가 `rustc_private` 사용
- blast/context/trace는 **함수/메서드 단위만**
- 현재 **Rust 전용**
