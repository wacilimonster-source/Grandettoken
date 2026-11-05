package com.wacil.grandettoken.sync.widget

import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.os.Bundle
import androidx.activity.ComponentActivity

/** Debug-only helper: pin request needs a foreground activity. */
class WidgetDemoPinActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
    }

    override fun onResume() {
        super.onResume()
        val provider = when (intent.getStringExtra("kind")) {
            "card" -> ComponentName(this, CardWidgetProvider::class.java)
            else -> ComponentName(this, StripWidgetProvider::class.java)
        }
        AppWidgetManager.getInstance(this).requestPinAppWidget(provider, null, null)
        finish()
    }
}
