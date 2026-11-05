# NFR-N1 WAN 측정 실행서

상태: 측정 준비 (#10, #229). 루프백 결과와 벤치 도구는 있지만 실제 WAN 매트릭스는 아직 검증하지 않았다. 소유자가 각 네트워크와 장비를 준비하고 결과를 판정한다. FR-N4 실물 전화 웹뷰 검증은 별도 작업이다.

## 장비와 네트워크

- 같은 리비전의 실행 준비된 `ember-transport-bench`가 있는 컴퓨터 두 대. 한 대는 listener, 다른 한 대는 dialer다. 각 기기의 OS, 버전, 커밋, 네트워크와 NAT 종류를 기록한다.
- 양쪽에서 접근 가능한 메인 서버와 인증된 장치 구성이 필요하다. 릴레이 검증에는 메인 서버의 TCP 릴레이와 UDP QAD 관찰자, 디렉터리와 rendezvous가 필요하다. 기본 포트는 TCP와 UDP 각각 7443이다. 서버와 기기의 도달성 및 허용 목록을 확인한다.
- 집 라우터 ↔ 학교 또는 사무실, 집 ↔ LTE 핫스팟, 한쪽이 대칭 NAT인 조합을 각각 준비한다. TCP는 허용하고 UDP는 차단한 학교 또는 사무실 조합도 기록한다. NAT 종류와 차단 정책을 확인할 수 없으면 해당 항목을 미검증으로 남긴다.
- Wi-Fi ↔ LTE 전환과 배터리 측정에는 Android와 iOS 실기기가 필요하다. 측정 환경과 충전 상태를 기록한다.

## 실행

각 조합에서 20회 실행한다. 저장소 루트에서 실행하고 기존 결과를 덮어쓰지 않도록 조합마다 다른 이름을 쓴다. 아래 경로와 IP는 환경에 맞게 바꾼다. listener는 20회가 끝날 때까지 유지한다.

1. 양쪽에서 기본 letify 백엔드를 사용하고, 필요한 주소 관찰 설정을 환경에 맞게 지정한다. listener에서 이미 준비한 실행 파일을 시작한다.

   ```sh
   mkdir -p .scratch/nfr-n1
   EMBER_TRANSPORT_BACKEND=letify /path/to/ember-transport-bench listen > .scratch/nfr-n1/listener.json
   ```

2. listener가 출력한 JSON 한 줄을 dialer의 `.scratch/nfr-n1/listener.json`으로 안전하게 전달한다. 주소에는 네트워크 정보가 포함되므로 공유 범위를 제한한다. raw TCP 기준 측정은 listener가 출력한 `tcp_port`에 dialer가 도달해야 한다. NAT와 방화벽 때문에 접근이 안 되면 같은 경로의 기준값을 확보하고 그 방법을 기록한다. 임의로 포트를 공개하면 측정 조건이 바뀌므로 변경 사항을 기록한다.
3. dialer에서 실행한다. `--tcp-host`는 listener의 raw TCP 포트에 도달하는 IP로 바꾼다. 스크립트의 추가 인자는 `dial`에 전달된다.

   ```sh
   BENCH_BIN=/path/to/ember-transport-bench EMBER_TRANSPORT_BACKEND=letify \
     .github/scripts/bench/run-matrix.sh home-office .scratch/nfr-n1/listener.json \
     .scratch/nfr-n1/results --tcp-host 192.0.2.10
   ```

4. 다른 조합도 같은 절차로 실행한다. 결과는 `.scratch/nfr-n1/results/<조합>/`의 `01.json`부터 `20.json`, 각 실행의 `.stderr`, 실패 시 `.exit`, `failures.txt`, `listener.json`에 남는다. 디렉터리가 이미 있으면 스크립트는 덮어쓰기를 거부한다. 실패도 분모에 포함하고 로그와 조건을 보존한다.
5. 각 JSON의 `setup_first_byte_ms`, `time_to_direct_ms`, `initial_path`, `path_during_pings`, `path_history`, `rtt`, `throughput`, `tcp_baseline`을 검토한다. `tcp_baseline` 오류나 null은 기준값 부재로 기록한다. 경로가 같은 raw TCP와 비교해야 한다. 측정 일시, 장비, NAT, 방화벽, 서버 설정, 실행 옵션과 함께 결과와 판정을 #10에 기록한다. 주소와 키 등 민감한 정보는 공개 전 제거한다.

6. 모든 조합이 끝나면 요약기로 표를 만든다. 요약기는 조합별 직접과 릴레이 비율, 첫 바이트 p50 및 p95, 직접 수립 p95, raw TCP 대비 RTT 차이와 처리량 비율, 릴레이에서 직접 전환 p95를 계산하고 20회 미만 조합, 실패, 없는 조합과 `tcp_baseline`이 없는 실행을 표시한다. 실패는 분모에 포함한다. 네트워크 전환, 직접에서 릴레이 복귀와 모바일 줄은 자동 데이터가 없어 항상 MISSING으로 나온다.

   ```sh
   uv run python -I .github/scripts/bench/summarize-matrix.py .scratch/nfr-n1/results \
     --expect home-office home-lte symmetric-nat udp-blocked
   ```

   조합 이름에 `sym`이 들어가면 대칭 NAT, `udp`가 들어가면 UDP 차단으로 본다. 다른 이름은 `--symmetric`, `--udp-blocked`로 지정한다. `--strict`는 표시할 항목이 있으면 종료 코드 1을 돌려준다. 요약기는 판정 보조일 뿐 합격을 선언하지 않으며, 테스트는 `bash .github/scripts/bench/tests/summarize-matrix.test.sh`다. p95는 정렬 후 `round((n - 1) * 0.95)`번째 값으로, 벤치와 같은 규칙이다.

이 벤치는 독립 실행 시 hub 디렉터리와 rendezvous를 설정하지 않는다. `--relay`만 지정해도 두 피어의 릴레이 페어링이 보장되지 않는다. 따라서 별도로 실제 Ember 장치와 메인 서버를 연결하여 릴레이, 경로 전환 및 복구를 측정하고 증거를 남긴다. 이 스크립트는 네트워크 전환, 배터리 소모 또는 합격 여부를 자동 측정하거나 판정하지 않는다. 필요한 수동 측정 도구나 절차가 없으면 해당 기준은 미검증으로 둔다.

## 판정

SPEC NFR-N1이 최종 기준이다. 각 조합의 20회와 실패를 포함하여 다음을 평가한다.

- 대칭 NAT를 제외한 전체 매트릭스의 직접 경로 성공률 ≥ 85%. 대칭 NAT는 양쪽이 메인 서버에 닿을 때 릴레이 연결 100%. 직접과 메인 서버 릴레이 비율을 따로 기록한다.
- UDP 차단 조합은 TCP 직접 또는 릴레이 연결 100%, 첫 바이트 p95 ≤ 3초. UDP 탐색 시간도 포함한다.
- 직접 경로 RTT는 같은 경로 raw TCP보다 p50 ≤ 5 ms, p95 ≤ 15 ms 추가. 처리량은 raw TCP의 ≥ 80%.
- 첫 바이트 p95 ≤ 1.5초, 직접 경로가 존재하면 수립 p95 ≤ 5초. 릴레이에서 직접 경로가 가능해진 뒤 전환 p95 ≤ 10초, 직접에서 릴레이로 돌아갈 때 스트림 리셋 없음.
- Wi-Fi ↔ LTE 전환 시 열린 스트림 유지, 중단 p95 ≤ 3초. Android와 iOS 각각 백그라운드 유휴 연결의 추가 배터리 소모 ≤ 2%/시간, 포그라운드 복귀 연결 p95 ≤ 1초.

20회 결과로 p95를 계산하고 계산 방법과 실패를 명시한다. 경로 전환, 모바일 및 릴레이 결과가 빠졌거나 기준 경로를 비교할 수 없으면 전체 합격으로 표시하지 않는다. 튜닝 후에도 기준을 놓치면 측정값과 함께 SPEC의 평가 요구를 따른다.
