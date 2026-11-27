#!/usr/bin/env bash
# Check that the development job exercises the release APK build path.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

ruby -ryaml <<'RUBY'
def check(condition, message)
  abort(message) unless condition
end

workflows = '.github/workflows'
dev = YAML.load_file("#{workflows}/test.yml").fetch('jobs')['android']
check(dev, 'test.yml must contain the Android APK job')
release = YAML.load_file("#{workflows}/publish-release.yml").fetch('jobs').fetch('android')
dispatch = YAML.load_file("#{workflows}/ci.yml").fetch('jobs')

check(dev.fetch('runs-on').start_with?('ubuntu-'), 'the Android APK job needs a Linux runner')
check(dev.fetch('env').fetch('NDK_VERSION') == '27.1.12297006', 'the Android NDK pin changed')
check(dev.fetch('steps').any? { |s| s.fetch('run', '').include?('sdkmanager" --install "ndk;$NDK_VERSION"') },
      'the Android APK job must install the pinned NDK')
check(dev.fetch('steps').any? { |s| s.dig('with', 'targets') == 'aarch64-linux-android' },
      'the Android APK job needs the arm64 Rust target')

command = './scripts/build-android.sh --release --cargo-args=--locked'
[dev, release].each do |job|
  step = job.fetch('steps').find { |s| s['name'] == 'Build the APK' }
  check(step && step.fetch('run').include?(command), 'both jobs must use the release APK command')
end
check(dev.fetch('steps').any? { |s| s.fetch('run', '').include?('lib/arm64-v8a/libmain.so') },
      'the Android APK job must verify the packaged arm64 library')
check(dispatch.fetch('test').fetch('uses') == './.github/workflows/test.yml' && !dispatch.key?('test_android'),
      'the normal development dispatch must run the APK job once through test.yml')

puts 'ok: development CI uses the pinned Linux Android SDK/NDK and release APK command'
RUBY
