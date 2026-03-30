# rude

Rust 코드 구조 분석 + 편집 CLI. MIR 기반 call graph로 100% 정확한 호출 관계 분석.

## 설치

```bash
cargo install --path crates/rude
```

nightly rustc 필요 (`mir-callgraph`가 `rustc_private` 사용).

## 빠른 시작

```bash
rude add .                              # DB 생성/갱신 (.code.db)
rude symbols --compact                  # 심볼 목록
rude context run -s                     # 함수 정의 + caller/callee + 소스
rude context run --blast                # 함수 변경 영향 범위 분석
rude context MyStruct --blast           # struct 필드 접근 기반 영향 범위
rude trace fn_a fn_b                    # 두 함수 간 최단 호출 경로
```

## 분석

```bash
rude cluster --file foo.rs              # 파일 내 함수 그룹 분석
rude dead                               # 호출자 없는 함수
rude dupes                              # 중복 코드 탐지
rude coverage --file foo.rs             # 테스트 커버리지
```

## 편집

```bash
# 심볼 기반
rude replace fn_name --body-file new.rs
rude delete-symbol fn_name
rude insert-after fn_name --body-file code.rs

# 모듈 분리
rude split-module --file foo.rs --auto                    # call graph 기반 자동 분리
rude split-module --file foo.rs "bar.rs:fn1,fn2" "baz.rs:fn3"  # 수동 지정
```

## 설정

`.code.db/config.toml`:

```toml
[split]
min_lines = 300

[cluster]
min_lines = 50
```
