---
name: rude-multilang-orchestrator
description: "rude 다국어 확장 + 출력 정리 오케스트레이터. Go VTA, TSC API extractor 구현과 출력 포맷 정리를 조율한다."
---

# 오케스트레이터: rude 다국어 확장

## 실행 모드
서브 에이전트 모드 — 3개 작업이 독립적이며 에이전트 간 통신 불필요.

## 실행 순서

### Phase 1: 출력 포맷 정리 (독립)
에이전트: `output-fixer`
- 구분선 통일, print 통일, stdout/stderr 분리
- 가장 간단하고 기존 코드만 수정
- 완료 후 `cargo nextest r` 검증

### Phase 2: Go Extractor 구현 (독립)
에이전트: `go-extractor`
1. `tools/go-callgraph/` 디렉토리 생성
2. `main.go` + `go.mod` 작성
3. `go build` 확인
4. bench-repos의 Go 프로젝트(gin, echo)로 테스트
5. rude-intel에 Go runner 추가
6. `install-rude.sh` 업데이트

### Phase 3: TSC Extractor 구현 (독립)
에이전트: `tsc-extractor`
1. `tools/ts-callgraph/` 디렉토리 생성
2. `index.ts` + `package.json` + `tsconfig.json` 작성
3. `npm install && npx tsc` 확인
4. bench-repos의 TS 프로젝트(zod, trpc)로 테스트
5. rude-intel에 TSC runner 추가
6. `install-rude.sh` 업데이트

### Phase 4: 통합 검증
- `cargo nextest r --status-level fail` — 기존 Rust 테스트 통과
- Go 프로젝트에 `rude add` → `rude context` 동작 확인
- TS 프로젝트에 `rude add` → `rude context` 동작 확인
- `rude stats` 출력에 Go/TS 크레이트가 표시되는지 확인

## 데이터 전달
파일 기반 — 각 에이전트가 직접 소스 파일을 생성/수정. 공유 상태 없음.

## 에러 핸들링
- Go/TS 빌드 실패 시 해당 Phase만 재시도 (다른 Phase에 영향 없음)
- 기존 Rust 테스트 실패 시 즉시 중단, 원인 분석
- bench-repos 테스트 실패 시 extractor 코드 수정 후 재시도 (최대 2회)

## 테스트 시나리오

### 정상 흐름
1. output-fixer가 출력 포맷 수정 → 테스트 통과
2. go-extractor가 Go 바이너리 생성 → gin 프로젝트에서 call graph 추출 성공
3. tsc-extractor가 TS 스크립트 생성 → zod 프로젝트에서 call graph 추출 성공
4. 전체 통합 테스트 통과

### 에러 흐름
1. Go 미설치 → go-extractor가 명확한 에러 메시지 출력, rude는 Rust-only 모드로 계속 동작
2. Node.js 미설치 → tsc-extractor 동일
3. Go 프로젝트에 빌드 에러 → go-callgraph가 파싱 가능한 패키지만 처리, 나머지 skip
