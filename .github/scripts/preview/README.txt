Ember 미리보기 (Windows)

이 묶음은 Ember 클라이언트를 데모 데이터와 함께 바로 확인하기 위한 미리보기입니다.

실행 방법
1. 압축을 풀고 run-preview.cmd를 더블 클릭합니다.
2. 서버(ember-server.exe)가 백그라운드에서 시작되고, 데모 프로젝트와 대화가 만들어진 뒤 ember-app.exe가 열립니다.
3. 앱을 닫으면 서버도 함께 종료됩니다.

참고
- 데이터는 %LOCALAPPDATA%\EmberPreview 에 저장됩니다. 지우려면 이 폴더를 삭제합니다.
- 서버는 127.0.0.1:8740 에서만 열립니다. 이미 사용 중이면 run-preview.cmd의 EMBER_LISTEN과 EMBER_SERVER_URL을 함께 바꿉니다.
- 에이전트는 실제 모델이 아니라 스크립트로 된 데모 에이전트입니다.
- 서명되지 않은 실행 파일이라 Windows SmartScreen 경고가 나올 수 있습니다. 자세히 보기에서 실행을 선택합니다.
