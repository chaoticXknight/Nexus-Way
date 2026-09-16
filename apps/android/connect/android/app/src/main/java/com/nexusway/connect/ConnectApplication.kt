package com.nexusway.connect

/**
 * Owns process startup work that must happen before any screen appears.
 * It creates notification infrastructure, but Nexus Notify owns the persistent
 * background connection and MainActivity owns visible UI lifecycle.
 */

import android.app.Application

class ConnectApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        CallSession.initialize(this)
        ConnectNotifications.createChannels(this)
        ConnectNotifications.schedule(this)
    }
}