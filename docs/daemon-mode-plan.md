# mir-callgraph daemon 모드

## 목표
- 증분 업데이트 0.5초 이내
- rustc_driver.dll(136MB) 로딩 1회만
- 이벤트 기반 (sleep/timer 없음)
- 순차 처리 (동시 쓰기 안전)

## 구현

### 1. mir-callgraph --daemon
- Named Pipe `\\.\pipe\rude-mir-{project_hash}` 생성
- 이벤트 루프: ConnectNamedPipe(블로킹) → ReadFile → 처리 → WriteFile → DisconnectNamedPipe → 반복
- 단일 스레드: 순차 처리 보장
- 유휴 시 CPU 0% (ConnectNamedPipe가 OS 레벨 블로킹)

### 2. 프로토콜 (JSON-line)
요청: {"cmd":"compile","args_file":"path","out_dir":"path","db":"path"}\n
응답: {"ok":true,"crate":"ide","chunks":1926,"edges":13418}\n

### 3. rude runner.rs 변경
1. pipe 연결 시도 (CreateFile)
2. 성공 → 요청 전송 → 응답 대기 → 파싱
3. 실패 → daemon 시작 (background) → 재시도 또는 subprocess fallback

### 4. 순차 처리 보장
- Named Pipe는 한 번에 하나의 클라이언트만 연결
- 두 번째 클라이언트는 ConnectNamedPipe에서 대기
- 처리 완료 후 DisconnectNamedPipe → 다음 클라이언트 연결

### 5. 유휴 상태
- 요청 없으면 ConnectNamedPipe에서 무한 대기 (이벤트 기반)
- sleep/timer 없음
- OS가 스레드를 대기 상태로 전환 (CPU 0%)
