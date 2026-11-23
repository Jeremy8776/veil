package io.github.jeremy8776.veil;

import android.Manifest;
import android.app.NativeActivity;
import android.content.*;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.hardware.camera2.*;
import android.media.*;
import android.net.wifi.WifiManager;
import android.os.*;
import android.security.keystore.*;
import android.util.*;
import android.view.Surface;
import java.nio.ByteBuffer;
import java.security.KeyStore;
import java.util.*;
import java.util.concurrent.atomic.AtomicReference;
import javax.crypto.*;
import javax.crypto.spec.GCMParameterSpec;

/** Native Veil UI plus Android-owned permissions, Camera2 capture and Keystore. */
public final class VeilActivity extends NativeActivity {
    private final AtomicReference<byte[]> newestFrame = new AtomicReference<>();
    private HandlerThread cameraThread;
    private Handler cameraHandler;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private ImageReader reader;
    private volatile int generation = 0;
    private volatile boolean resumed = false;
    private volatile boolean backRequested = false;
    private volatile int topInset = 0, bottomInset = 0, leftInset = 0, rightInset = 0;
    private final AtomicReference<String> pendingInvite = new AtomicReference<>("");
    private WifiManager.MulticastLock multicast;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        readInvite(getIntent());
        if (Build.VERSION.SDK_INT >= 33) getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
            android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT, () -> backRequested = true);
        getWindow().addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            if (Build.VERSION.SDK_INT >= 30) {
                android.graphics.Insets bars = insets.getInsets(android.view.WindowInsets.Type.systemBars()
                    | android.view.WindowInsets.Type.displayCutout());
                topInset = bars.top; bottomInset = bars.bottom;
                bottomInset = Math.max(bottomInset, insets.getInsets(android.view.WindowInsets.Type.ime()).bottom);
                leftInset = bars.left; rightInset = bars.right;
            } else {
                topInset = insets.getSystemWindowInsetTop(); bottomInset = insets.getSystemWindowInsetBottom();
                leftInset = insets.getSystemWindowInsetLeft(); rightInset = insets.getSystemWindowInsetRight();
            }
            return insets;
        });
        WifiManager wifi = (WifiManager)getApplicationContext().getSystemService(WIFI_SERVICE);
        if (wifi != null) { multicast = wifi.createMulticastLock("veil-nearby"); multicast.setReferenceCounted(false); }
    }

    @Override public void onResume() {
        super.onResume(); resumed = true;
        if (multicast != null && !multicast.isHeld()) multicast.acquire();
    }
    @Override public void onPause() {
        resumed = false;
        veilCloseCamera();
        if (multicast != null && multicast.isHeld()) multicast.release();
        super.onPause();
    }
    @Override public void onDestroy() {
        veilCloseCamera();
        stopService(new Intent(this, CallService.class));
        if (cameraThread != null) cameraThread.quitSafely();
        super.onDestroy();
        // winit supports one event loop per process. At this point NativeActivity has
        // finished its lifecycle and our camera and call service are already stopped.
        // Discard this dedicated app runtime so a later launch gets a fresh event loop.
        android.os.Process.killProcess(android.os.Process.myPid());
    }
    @Override public void onBackPressed() { backRequested = true; }
    @Override public void onNewIntent(Intent intent) { super.onNewIntent(intent); readInvite(intent); }
    private void readInvite(Intent intent) {
        String value = intent.getDataString();
        if (Intent.ACTION_SEND.equals(intent.getAction())) value = intent.getStringExtra(Intent.EXTRA_TEXT);
        if (value != null && value.length() <= 8192 && value.startsWith("veil:")) pendingInvite.set(value);
    }
    public String veilTakeInvite() { return pendingInvite.getAndSet(""); }
    public long veilInsets() { return ((long)topInset << 32) | (bottomInset & 0xffffffffL); }
    public long veilSideInsets() { return ((long)leftInset << 32) | (rightInset & 0xffffffffL); }
    public float veilFontScale() { return getResources().getConfiguration().fontScale; }
    public void veilBackground() { runOnUiThread(() -> moveTaskToBack(true)); }
    public boolean veilBackRequested() { boolean value = backRequested; backRequested = false; return value; }
    public boolean veilResumed() { return resumed; }
    public boolean veilPermissionsReady() {
        return veilPermissionState() == 3;
    }
    public int veilPermissionState() {
        return (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED ? 1 : 0)
            | (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED ? 2 : 0);
    }
    public String veilDataDir() { return getFilesDir().getAbsolutePath(); }

    public void veilNotify(String heading, String body) {
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return;
        android.app.NotificationManager notices = getSystemService(android.app.NotificationManager.class);
        notices.createNotificationChannel(new android.app.NotificationChannel("veil-lobby", "Call lobby", android.app.NotificationManager.IMPORTANCE_DEFAULT));
        android.app.PendingIntent open = android.app.PendingIntent.getActivity(this, 0, new Intent(this, VeilActivity.class), android.app.PendingIntent.FLAG_UPDATE_CURRENT | android.app.PendingIntent.FLAG_IMMUTABLE);
        notices.notify(2, new android.app.Notification.Builder(this, "veil-lobby").setSmallIcon(android.R.drawable.sym_call_incoming)
            .setContentTitle(heading).setContentText(body).setContentIntent(open).setAutoCancel(true).build());
    }
    public void veilClearNotifications() { getSystemService(android.app.NotificationManager.class).cancel(2); }

    public void veilRequestPermissions() {
        runOnUiThread(() -> {
            ArrayList<String> needed = new ArrayList<>();
            for (String permission : new String[]{Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO})
                if (checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) needed.add(permission);
            if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED)
                needed.add(Manifest.permission.POST_NOTIFICATIONS);
            if (!needed.isEmpty()) requestPermissions(needed.toArray(new String[0]), 1);
        });
    }

    public void veilCallActive(boolean active) {
        runOnUiThread(() -> {
            Intent intent = new Intent(this, CallService.class);
            if (active) startForegroundService(intent); else stopService(intent);
            AudioManager audio = getSystemService(AudioManager.class);
            audio.setMode(active ? AudioManager.MODE_IN_COMMUNICATION : AudioManager.MODE_NORMAL);
        });
    }

    public String veilClipboard(String value, boolean copy) {
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        if (copy) { clipboard.setPrimaryClip(ClipData.newPlainText("Veil invite", value)); return ""; }
        ClipData clip = clipboard.getPrimaryClip();
        return clip != null && clip.getItemCount() > 0 ? clip.getItemAt(0).coerceToText(this).toString() : "";
    }

    public synchronized byte[] veilProtect(byte[] bytes, byte[] label, boolean seal) throws Exception {
        try {
        String alias = "veil-device-storage-v1";
        KeyStore store = KeyStore.getInstance("AndroidKeyStore"); store.load(null);
        if (!store.containsAlias(alias)) {
            if (!seal) throw new java.security.GeneralSecurityException("Veil storage key is missing");
            KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            generator.init(new KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true).build());
            generator.generateKey();
        }
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        java.security.Key key = store.getKey(alias, null);
        if (seal) {
            cipher.init(Cipher.ENCRYPT_MODE, key); cipher.updateAAD(label);
            byte[] ciphertext = cipher.doFinal(bytes), iv = cipher.getIV();
            return ByteBuffer.allocate(1 + iv.length + ciphertext.length).put((byte)iv.length).put(iv).put(ciphertext).array();
        }
        if (bytes.length < 29 || bytes[0] != 12) throw new java.security.GeneralSecurityException("Invalid Veil storage record");
        cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, bytes, 1, 12));
        cipher.updateAAD(label);
        return cipher.doFinal(bytes, 13, bytes.length - 13);
        } finally { Arrays.fill(bytes, (byte)0); }
    }

    public String veilCameras() throws CameraAccessException {
        CameraManager manager = getSystemService(CameraManager.class);
        StringBuilder result = new StringBuilder();
        for (String id : manager.getCameraIdList()) {
            Integer facing = manager.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING);
            result.append(id).append('\t').append(facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT ? "Front camera" : "Rear camera").append('\n');
        }
        return result.toString();
    }

    public synchronized boolean veilOpenCamera(String wanted) throws CameraAccessException {
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED || !resumed) return false;
        veilCloseCamera();
        if (cameraThread == null) { cameraThread = new HandlerThread("veil-camera"); cameraThread.start(); cameraHandler = new Handler(cameraThread.getLooper()); }
        CameraManager manager = getSystemService(CameraManager.class);
        String chosen = wanted;
        if (chosen.isEmpty()) {
            for (String id : manager.getCameraIdList()) {
                Integer facing = manager.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING);
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT) { chosen = id; break; }
            }
            if (chosen.isEmpty() && manager.getCameraIdList().length > 0) chosen = manager.getCameraIdList()[0];
        }
        if (chosen.isEmpty()) return false;
        CameraCharacteristics info = manager.getCameraCharacteristics(chosen);
        Size[] sizes = info.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP).getOutputSizes(ImageFormat.YUV_420_888);
        Size size = Arrays.stream(sizes).filter(s -> s.getWidth() <= 960 && s.getHeight() <= 720 && s.getWidth() % 2 == 0 && s.getHeight() % 2 == 0)
            .min(Comparator.comparingLong(s -> Math.abs((long)s.getWidth() * s.getHeight() - 640L * 480))).orElse(null);
        if (size == null) return false;
        int sensor = info.get(CameraCharacteristics.SENSOR_ORIENTATION);
        boolean front = info.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_FRONT;
        final int opening = generation;
        reader = ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, 3);
        ImageReader target = reader;
        target.setOnImageAvailableListener(source -> {
            try (Image image = source.acquireLatestImage()) {
                if (image == null || opening != generation) return;
                int display = getWindowManager().getDefaultDisplay().getRotation() * 90;
                int rotation = (sensor + (front ? display : -display) + 360) % 360;
                byte[] packed = packFrame(image, rotation);
                synchronized (VeilActivity.this) {
                    // Closing or switching the camera may happen while planes are copied.
                    if (opening == generation && resumed) newestFrame.set(packed);
                }
            } catch (IllegalStateException problem) { Log.w("Veil", "Camera frame unavailable"); }
        }, cameraHandler);
        manager.openCamera(chosen, new CameraDevice.StateCallback() {
            @Override public void onOpened(CameraDevice opened) {
                synchronized (VeilActivity.this) {
                    if (opening != generation || !resumed) { opened.close(); return; }
                    camera = opened;
                    try {
                        opened.createCaptureSession(Collections.singletonList(target.getSurface()), new CameraCaptureSession.StateCallback() {
                            @Override public void onConfigured(CameraCaptureSession configured) {
                                synchronized (VeilActivity.this) {
                                    if (opening != generation) { configured.close(); return; }
                                    session = configured;
                                    try {
                                        CaptureRequest.Builder request = opened.createCaptureRequest(CameraDevice.TEMPLATE_RECORD);
                                        request.addTarget(target.getSurface());
                                        request.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO);
                                        configured.setRepeatingRequest(request.build(), null, cameraHandler);
                                    } catch (CameraAccessException | IllegalStateException problem) { Log.e("Veil", "Cannot start camera", problem); }
                                }
                            }
                            @Override public void onConfigureFailed(CameraCaptureSession failed) { failed.close(); }
                        }, cameraHandler);
                    } catch (CameraAccessException problem) { opened.close(); }
                }
            }
            @Override public void onDisconnected(CameraDevice disconnected) { disconnected.close(); }
            @Override public void onError(CameraDevice failed, int error) { failed.close(); Log.e("Veil", "Camera error " + error); }
        }, cameraHandler);
        return true;
    }

    public byte[] veilCameraFrame() { return newestFrame.getAndSet(null); }

    public synchronized void veilCloseCamera() {
        generation++;
        if (session != null) { session.close(); session = null; }
        if (camera != null) { camera.close(); camera = null; }
        if (reader != null) { reader.close(); reader = null; }
        newestFrame.set(null);
    }

    /** Preserve each plane's row/pixel stride and rotate both luma and chroma. */
    static byte[] packFrame(Image image, int rotation) {
        int width = image.getWidth(), height = image.getHeight();
        int outWidth = rotation % 180 == 0 ? width : height, outHeight = rotation % 180 == 0 ? height : width;
        byte[] out = new byte[4 + width * height * 3 / 2];
        out[0] = (byte)outWidth; out[1] = (byte)(outWidth >> 8); out[2] = (byte)outHeight; out[3] = (byte)(outHeight >> 8);
        Image.Plane[] planes = image.getPlanes();
        for (int plane = 0; plane < 3; plane++) {
            int w = plane == 0 ? width : width / 2, h = plane == 0 ? height : height / 2;
            int ow = rotation % 180 == 0 ? w : h;
            ByteBuffer buffer = planes[plane].getBuffer();
            int base = buffer.position(), row = planes[plane].getRowStride(), pixel = planes[plane].getPixelStride();
            for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
                int ox = x, oy = y;
                if (rotation == 90) { ox = h - 1 - y; oy = x; }
                else if (rotation == 180) { ox = w - 1 - x; oy = h - 1 - y; }
                else if (rotation == 270) { ox = y; oy = w - 1 - x; }
                int at = plane == 0 ? 4 + oy * ow + ox : 4 + width * height + (oy * ow + ox) * 2 + plane - 1;
                out[at] = buffer.get(base + y * row + x * pixel);
            }
        }
        return out;
    }
}
