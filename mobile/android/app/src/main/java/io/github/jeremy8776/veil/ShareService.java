package io.github.jeremy8776.veil;

import android.app.*;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.os.Build;
import android.os.IBinder;

/**
 * The foreground service of type mediaProjection that a screen share needs. Its notification
 * says the screen is being shared and has Stop sharing. Stopping the service, removing the
 * task or pressing Stop all end the share (ScreenShare.java).
 */
public final class ShareService extends Service {
    static final String STOP = "io.github.jeremy8776.veil.STOP_SHARE";
    static final String CODE = "code", CONSENT = "consent", GENERATION = "generation";

    @SuppressWarnings("deprecation")
    @Override public int onStartCommand(Intent intent, int flags, int id) {
        if (intent == null || STOP.equals(intent.getAction())) {
            ScreenShare.stopAll();
            stopSelf();
            return START_NOT_STICKY;
        }
        String channel = "veil-share";
        getSystemService(NotificationManager.class).createNotificationChannel(
            new NotificationChannel(channel, "Screen sharing", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, VeilActivity.class),
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        PendingIntent stop = PendingIntent.getService(this, 1, new Intent(this, ShareService.class).setAction(STOP),
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Notification notice = new Notification.Builder(this, channel)
            .setSmallIcon(android.R.drawable.ic_menu_slideshow).setContentTitle("Sharing your screen")
            .setContentText("Everyone in the call can see your screen").setContentIntent(open).setOngoing(true)
            .addAction(new Notification.Action.Builder(
                Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel), "Stop sharing", stop).build())
            .build();
        int generation = intent.getIntExtra(GENERATION, -1);
        try { startForeground(3, notice, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION); }
        catch (RuntimeException problem) { ScreenShare.denied(generation); stopSelf(); return START_NOT_STICKY; }
        Intent consent = Build.VERSION.SDK_INT >= 33
            ? intent.getParcelableExtra(CONSENT, Intent.class) : intent.getParcelableExtra(CONSENT);
        if (!ScreenShare.begin(this, generation, intent.getIntExtra(CODE, 0), consent)) {
            ScreenShare.denied(generation);
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
        }
        return START_NOT_STICKY;
    }

    @Override public void onTaskRemoved(Intent root) { ScreenShare.stopAll(); stopSelf(); }
    @Override public void onDestroy() { ScreenShare.serviceGone(this); super.onDestroy(); }
    @Override public IBinder onBind(Intent intent) { return null; }
}
