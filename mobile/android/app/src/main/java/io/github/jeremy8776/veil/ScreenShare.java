package io.github.jeremy8776.veil;

import android.Manifest;
import android.app.Service;
import android.content.*;
import android.content.pm.PackageManager;
import android.graphics.PixelFormat;
import android.hardware.display.*;
import android.media.*;
import android.media.projection.*;
import android.os.*;
import android.util.*;
import android.view.Display;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.atomic.AtomicReference;

/**
 * Screen sharing (SHARE-02). Java owns the MediaProjection, its virtual display, the image
 * reader and playback capture; Rust polls copies by generation (ffi/android_share.rs).
 *
 * Invariants:
 * - Each consent prompt gets a new generation. Anything asked for an older one gets nothing,
 *   and a frame copied for an older one is dropped under the same lock that stops it.
 * - Stopping, by any route (Rust, the notification, the system indicator, revoked consent,
 *   screen off, task removal, Activity destruction), releases the display, reader, sound
 *   capture, projection and foreground service together. Stopping twice is harmless.
 * - Pictures are copied into a fresh array with a 4-byte size header, at most MAX_SIDE on
 *   the longest side. Sound is copied out as 16-bit little-endian stereo.
 */
final class ScreenShare {
    static final int WAITING = 0, LIVE = 1, DENIED = 2, ENDED = 3;
    /** Longest side of the shared picture; within Rust's projection::MAX_SIDE. */
    private static final int MAX_SIDE = 1280;
    /** Bytes of sound per read: 80 ms of 16-bit stereo at 48 kHz, Rust's MAX_SOUND_BYTES. */
    private static final int SOUND_BYTES = 48 * 80 * 2 * 2;

    private static final Object lock = new Object();
    private static final AtomicReference<byte[]> newest = new AtomicReference<>();
    private static final short[] soundBuffer = new short[SOUND_BYTES / 2];
    private static int generation = 0;
    private static int state = ENDED;
    private static Service service;
    private static MediaProjection projection;
    private static VirtualDisplay display;
    private static ImageReader reader;
    private static HandlerThread thread;
    private static AudioRecord record;
    private static BroadcastReceiver screenOff;

    private ScreenShare() {}

    /** A consent prompt is about to open. Ends any earlier share. */
    static int request() {
        synchronized (lock) {
            stopLocked();
            generation++;
            state = WAITING;
            return generation;
        }
    }

    static void denied(int asked) {
        synchronized (lock) {
            if (asked == generation && state == WAITING) state = DENIED;
        }
    }

    static int status(int asked) {
        synchronized (lock) { return asked == generation ? state : ENDED; }
    }

    static byte[] frame(int asked) {
        synchronized (lock) {
            if (asked != generation || state != LIVE) return null;
        }
        return newest.getAndSet(null);
    }

    static void stop(int asked) {
        synchronized (lock) { if (asked == generation) stopLocked(); }
    }

    static void stopAll() {
        synchronized (lock) { stopLocked(); }
    }

    /** The foreground service has gone; nothing may keep capturing without it. */
    static void serviceGone(Service gone) {
        synchronized (lock) { if (service == gone) stopLocked(); }
    }

    /** Starts the projection from the service, which is already in the foreground. */
    @SuppressWarnings("deprecation")
    static boolean begin(Service owner, int asked, int code, Intent consent) {
        synchronized (lock) {
            if (asked != generation || state != WAITING || consent == null) return false;
            MediaProjectionManager manager = owner.getSystemService(MediaProjectionManager.class);
            MediaProjection started;
            try { started = manager.getMediaProjection(code, consent); }
            catch (RuntimeException problem) { Log.w("Veil", "Screen share refused"); started = null; }
            if (started == null) { state = DENIED; return false; }
            service = owner;
            projection = started;
            thread = new HandlerThread("veil-share");
            thread.start();
            Handler handler = new Handler(thread.getLooper());
            final int live = asked;
            started.registerCallback(new MediaProjection.Callback() {
                @Override public void onStop() { stop(live); }
            }, handler);
            Display screen = owner.getSystemService(DisplayManager.class).getDisplay(Display.DEFAULT_DISPLAY);
            DisplayMetrics metrics = new DisplayMetrics();
            screen.getRealMetrics(metrics);
            // shortcut: the display keeps the size it starts with, so turning the phone or
            // sharing one app of another shape letterboxes the picture. Resize on
            // onCapturedContentResize (Android 14+) and on rotation when that matters.
            float scale = Math.min(1f, MAX_SIDE / (float)Math.max(metrics.widthPixels, metrics.heightPixels));
            int width = Math.max(16, (int)(metrics.widthPixels * scale) & ~1);
            int height = Math.max(16, (int)(metrics.heightPixels * scale) & ~1);
            try {
                reader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 2);
                reader.setOnImageAvailableListener(source -> publish(live, source), handler);
                display = started.createVirtualDisplay("veil-share", width, height, metrics.densityDpi,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, reader.getSurface(), null, handler);
            } catch (RuntimeException problem) {
                Log.w("Veil", "Screen share display failed");
                stopLocked();
                return false;
            }
            screenOff = new BroadcastReceiver() {
                @Override public void onReceive(Context context, Intent intent) { stop(live); }
            };
            IntentFilter filter = new IntentFilter(Intent.ACTION_SCREEN_OFF);
            if (Build.VERSION.SDK_INT >= 33) owner.registerReceiver(screenOff, filter, Context.RECEIVER_NOT_EXPORTED);
            else owner.registerReceiver(screenOff, filter);
            state = LIVE;
            return true;
        }
    }

    private static void publish(int live, ImageReader source) {
        try (Image image = source.acquireLatestImage()) {
            if (image == null) return;
            int width = image.getWidth(), height = image.getHeight();
            Image.Plane plane = image.getPlanes()[0];
            if (plane.getPixelStride() != 4 || width > MAX_SIDE || height > MAX_SIDE) return;
            ByteBuffer buffer = plane.getBuffer();
            int row = plane.getRowStride();
            byte[] out = new byte[4 + width * height * 4];
            out[0] = (byte)width; out[1] = (byte)(width >> 8); out[2] = (byte)height; out[3] = (byte)(height >> 8);
            for (int y = 0; y < height; y++) {
                buffer.position(y * row);
                buffer.get(out, 4 + y * width * 4, width * 4);
            }
            synchronized (lock) {
                // Stopping may happen while rows are copied.
                if (live == generation && state == LIVE) newest.set(out);
            }
        } catch (RuntimeException problem) { Log.w("Veil", "Screen picture unavailable"); }
    }

    static boolean soundStart(Context context, int asked) {
        synchronized (lock) {
            if (asked != generation || state != LIVE || projection == null || record != null) return false;
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) return false;
            try {
                // Call audio uses the voice-communication usage, which is never matched, and
                // Veil's own app is excluded, so nobody hears the call echoed back.
                AudioPlaybackCaptureConfiguration config = new AudioPlaybackCaptureConfiguration.Builder(projection)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA).addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN).excludeUid(android.os.Process.myUid()).build();
                AudioFormat format = new AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(48000).setChannelMask(AudioFormat.CHANNEL_IN_STEREO).build();
                int minimum = AudioRecord.getMinBufferSize(48000, AudioFormat.CHANNEL_IN_STEREO, AudioFormat.ENCODING_PCM_16BIT);
                AudioRecord made = new AudioRecord.Builder().setAudioFormat(format)
                    .setBufferSizeInBytes(Math.max(minimum, SOUND_BYTES * 2)).setAudioPlaybackCaptureConfig(config).build();
                if (made.getState() != AudioRecord.STATE_INITIALIZED) { made.release(); return false; }
                made.startRecording();
                if (made.getRecordingState() != AudioRecord.RECORDSTATE_RECORDING) { made.release(); return false; }
                record = made;
                return true;
            } catch (RuntimeException problem) {
                Log.w("Veil", "Phone sound capture refused");
                return false;
            }
        }
    }

    /** Sound since the last read, possibly empty; null once sound capture has ended. */
    static byte[] soundRead(int asked) {
        synchronized (lock) {
            if (asked != generation || state != LIVE || record == null) return null;
            int read = record.read(soundBuffer, 0, soundBuffer.length, AudioRecord.READ_NON_BLOCKING);
            if (read < 0) return null;
            byte[] out = new byte[read * 2];
            ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer().put(soundBuffer, 0, read);
            return out;
        }
    }

    static void soundStop(int asked) {
        synchronized (lock) { if (asked == generation) soundStopLocked(); }
    }

    private static void soundStopLocked() {
        if (record == null) return;
        try { record.stop(); } catch (IllegalStateException ignored) { }
        record.release();
        record = null;
    }

    private static void stopLocked() {
        if (state == WAITING || state == LIVE) state = ENDED;
        soundStopLocked();
        if (display != null) { display.release(); display = null; }
        if (reader != null) { reader.close(); reader = null; }
        if (projection != null) { MediaProjection ending = projection; projection = null; ending.stop(); }
        if (thread != null) { thread.quitSafely(); thread = null; }
        newest.set(null);
        if (service != null) {
            Service ending = service;
            service = null;
            if (screenOff != null) {
                try { ending.unregisterReceiver(screenOff); } catch (IllegalArgumentException ignored) { }
            }
            ending.stopForeground(Service.STOP_FOREGROUND_REMOVE);
            ending.stopSelf();
        }
        screenOff = null;
    }
}
