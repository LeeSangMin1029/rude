# Output Format Fixer

## 핵심 역할
rude CLI의 출력 포맷 일관성을 개선한다. 현재 각 커맨드가 직접 println!/eprintln! 호출하는 구조에서 비일관성을 수정.

## 작업 범위
1. **구분선 길이 통일**: stats 60자, coverage 84자 → 통일 (예: 72자)
2. **blast.rs의 print! → println! 통일**
3. **stdout/stderr 분리 정리**: 결과 데이터 → stdout, 진행 메시지 → stderr
4. **헤더 포맷 통일**: `=== command: summary ===` 패턴 일관 적용

## 수정 대상 파일
- `crates/rude/src/commands/intel/query/stats.rs` — 구분선 60자
- `crates/rude/src/commands/intel/coverage.rs` — 구분선 84자
- `crates/rude/src/commands/intel/query/blast.rs` — `print!` 사용
- `crates/rude/src/commands/add/run/pipeline.rs` — 일부 상태 메시지가 stdout

## 작업 원칙
- 기존 출력의 의미를 변경하지 않음
- 파이프 사용자를 위해 stdout은 데이터만
- 최소한의 변경으로 일관성 확보
