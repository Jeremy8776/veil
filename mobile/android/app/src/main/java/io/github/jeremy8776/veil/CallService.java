package io.github.jeremy8776.veil;

import android.app.*;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;

/** Keeps an explicitly started call visible to Android while the app is in the background. */
public final class CallService extends Service {
    @Override public void onCreate() {
        super.onCreate();
        String channel = "veil-calls";
        getSystemService(NotificationManager.class).createNotificationChannel(
            new NotificationChannel(channel, "Ongoing calls", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, VeilActivity.class),
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Notification notice = new Notification.Builder(this, channel)
            .setSmallIcon(android.R.drawable.sym_call_incoming).setContentTitle("Veil call")
            .setContentText("Tap to return to your call").setContentIntent(open).setOngoing(true).build();
        int types = 0;
        if (checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) == android.content.pm.PackageManager.PERMISSION_GRANTED)
            types |= ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE;
        if (checkSelfPermission(android.Manifest.permission.CAMERA) == android.content.pm.PackageManager.PERMISSION_GRANTED)
            types |= ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA;
        if (types == 0) { stopSelf(); return; }
        if (android.os.Build.VERSION.SDK_INT >= 30) startForeground(1, notice, types);
        else startForeground(1, notice);
    }
    @Override public int onStartCommand(Intent intent, int flags, int id) { return START_NOT_STICKY; }
    @Override public IBinder onBind(Intent intent) { return null; }
}
