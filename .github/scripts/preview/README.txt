Ember 미리보기 (Windows)

이 묶음은 Ember 클라이언트(ember-app.exe)만 담고 있습니다. 서버는 macOS와 Linux에서만 제공되므로, 데모 서버는 Mac이나 Linux 컴퓨터에서 따로 실행합니다.

1. 서버 컴퓨터(Mac 또는 Linux)에서 데모 서버를 실행합니다.
   EMBER_DEMO_DATA=1 EMBER_SCRIPTED_AGENT=1 EMBER_LISTEN=0.0.0.0:8740 ember-server

2. 같은 컴퓨터의 다른 터미널에서 데모 프로젝트와 대화를 만듭니다.
   curl -X POST http://127.0.0.1:8740/api/projects -H "Content-Type: application/json" -d '{"name":"demo-project"}'
   curl -X POST http://127.0.0.1:8740/api/sessions -H "Content-Type: application/json" -d '{"project":"demo-project","agent":"scripted","cwd":"/tmp","title":"Demo"}'

3. Windows에서 압축을 풀고 run-preview.cmd를 실행합니다. 서버 주소를 물으면 http://<서버 컴퓨터의 IP>:8740 을 입력합니다.
   명령줄에서는 run-preview.cmd http://<서버 컴퓨터의 IP>:8740 처럼 주소를 바로 넘길 수 있습니다.

참고
- 에이전트는 실제 모델이 아니라 스크립트로 된 데모 에이전트입니다.
- 두 컴퓨터가 같은 네트워크에 있어야 하고, 서버 컴퓨터의 방화벽에서 8740 포트를 허용해야 합니다.
- 서명되지 않은 실행 파일이라 Windows SmartScreen 경고가 나올 수 있습니다. 자세히 보기에서 실행을 선택합니다.
