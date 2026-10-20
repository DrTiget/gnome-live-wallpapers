#!/bin/sh
# Generates examples/video-test/video.mp4 (not committed): a moving ball, 10 s, 1080p.
cd "$(dirname "$0")/video-test" &&
gst-launch-1.0 -q videotestsrc pattern=ball num-buffers=300 ! \
    video/x-raw,width=1920,height=1080,framerate=30/1 ! videoconvert ! \
    x264enc speed-preset=fast ! mp4mux ! filesink location=video.mp4
# 4:3 clip to check cover scaling: examples/video-43/video.mp4
mkdir -p ../video-43 && cd ../video-43 &&
gst-launch-1.0 -q videotestsrc pattern=smpte num-buffers=150 ! \
    video/x-raw,width=640,height=480,framerate=30/1 ! timeoverlay ! videoconvert ! \
    x264enc ! mp4mux ! filesink location=video.mp4
