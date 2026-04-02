# Go Call Graph Extractor

## 핵심 역할
Go 프로젝트의 call graph를 VTA 알고리즘으로 추출하는 Go 바이너리(`go-callgraph`)를 구현한다.
`tools/go-callgraph/` 디렉토리에 독립 Go 모듈로 생성.

## 작업 원칙
- `golang.org/x/tools/go/callgraph/vta` 사용 — interface dispatch까지 정확히 resolve
- 출력: JSON `[]CallEdge` (stdout) — rude가 파싱해서 mir.db에 저장
- rude-intel의 `MirChunk`/`CallEdge` 스키마와 호환되는 포맷
- Go 1.21+ 기준, `go.mod` 포함

## 출력 포맷
```json
[
  {"caller":"(*pkg.Server).Handle","callee":"pkg.NewRouter","file":"server.go","line":42,"caller_file":"server.go","caller_start":30,"caller_end":55}
]
```

## 입력
- CLI args: `go-callgraph [--json] ./...` (Go 패키지 패턴)
- stdout: JSON CallEdge 배열
- stderr: 진행 메시지

## rude 통합 포인트
- `crates/rude-intel/src/mir_edges/runner.rs` — Go extractor 호출 로직 추가
- `crates/rude/src/commands/add/run/pipeline.rs` — `--lang go` 감지 시 Go extractor 실행
- `install-rude.sh` — Go extractor 빌드/설치 추가

## 에러 핸들링
- Go 미설치 시 명확한 에러 메시지 출력
- 파싱 실패한 패키지는 skip하고 나머지 계속 진행
- exit code 0 = 성공, 1 = 부분 실패, 2 = 전체 실패
