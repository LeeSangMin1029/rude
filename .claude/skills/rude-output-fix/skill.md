---
name: rude-output-fix
description: "rude CLI 출력 포맷 일관성 개선. 구분선 길이 통일, stdout/stderr 분리, print!/println! 통일. 'output fix', '출력 정리', '포맷 통일' 요청 시 사용."
---

# rude 출력 포맷 정리

## 변경 목록

### 1. 구분선 길이 통일 → 72자
- `stats.rs:20` — `"-".repeat(60)` → `"-".repeat(72)`
- `stats.rs:26` — `"-".repeat(60)` → `"-".repeat(72)`
- `coverage.rs:39` — `"-".repeat(84)` → `"-".repeat(72)`
- `coverage.rs:58` — `"-".repeat(84)` → `"-".repeat(72)`
- stats 컬럼 폭도 72자에 맞게 조정

### 2. blast.rs `print!` → `println!`
- `blast.rs:19` — `print!("=== context: ...")` → `println!("=== context: ...")`
- `blast.rs:36` — 동일
- `blast.rs:95` — 동일

### 3. pipeline.rs stdout/stderr 분리
- `pipeline.rs:24-25` — `println!("Indexing code: ...")` → `eprintln!` (진행 메시지)
- `pipeline.rs:68` — `println!("Files: ...")` → `eprintln!`
- `pipeline.rs:74` — `println!("New database: ...")` → `eprintln!`
- `pipeline.rs:122` — `println!("Inserted ...")` → `eprintln!`
- `pipeline.rs:156-158` — `println!("No changes..." / "Done!")` → `eprintln!`
- 최종 결과 (chunks 데이터)만 stdout에 유지

## 검증
- `cargo nextest r --status-level fail` — 전 테스트 통과
- `rude stats`, `rude cov`, `rude context` 출력 확인
- `rude add . 2>/dev/null` — stdout에 불필요한 출력이 없는지 확인
