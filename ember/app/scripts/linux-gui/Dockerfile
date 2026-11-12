FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb openbox xdotool imagemagick libgl1-mesa-dri libgl1-mesa-dev \
    libx11-dev libxext-dev libfontconfig1-dev libfreetype-dev fonts-noto-cjk \
    ca-certificates git && rm -rf /var/lib/apt/lists/*
WORKDIR /work/ember/app
ENV CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/target LIBGL_ALWAYS_SOFTWARE=1 DISPLAY=:99 EMBER_DEMO_DATA=1
