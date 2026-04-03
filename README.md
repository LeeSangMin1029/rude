# rude

코드 구조 분석 + 편집 CLI. Rust(MIR), Go, TypeScript 지원.

## 설치

```bash
bash install-rude.sh
```

nightly rustc, Go SDK, Node.js는 자동 설치/감지됩니다.

## 빠른 시작

```bash
rude add .                              # 프로젝트 인덱싱 (언어 자동 감지)
rude symbols search                     # 심볼 검색 (부분 매칭)
rude context run -s                     # 함수 정의 + caller/callee + 소스
rude context MyStruct --blast           # struct 변경 영향 범위
rude trace fn_a fn_b                    # 두 함수 간 최단 호출 경로
```

## 분석

```bash
rude dead                               # 호출자 없는 함수
rude dupes                              # 중복 코드 탐지
rude cluster --file foo.rs              # 파일 내 함수 그룹 분석
rude coverage --file foo.rs             # 테스트 커버리지 (Rust)
```

## 편집

```bash
rude replace fn_name --body-file new.rs
rude delete-symbol fn_name
rude insert-after fn_name --body-file code.rs
rude split-module --file foo.rs --auto  # call graph 기반 자동 모듈 분리
```

## 다국어 지원

| 언어 | 감지 | 분석 방식 |
|------|------|----------|
| Rust | `Cargo.toml` | MIR call graph (100% 정확) |
| Go | `go.mod` | go/ast + go/parser |
| TypeScript | `tsconfig.json` / `package.json` | TSC Compiler API |

## 설정

`.code.db/config.toml`:

```toml
[split]
min_lines = 300

[cluster]
min_lines = 50
```
