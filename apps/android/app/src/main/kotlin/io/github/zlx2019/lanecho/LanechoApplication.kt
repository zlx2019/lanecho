package io.github.zlx2019.lanecho

import android.app.Application
import android.os.Build
import io.github.zlx2019.lanecho.core.sync.Engine
import io.github.zlx2019.lanecho.core.util.Log
import io.github.zlx2019.lanecho.platform.AndroidImageCodec
import io.github.zlx2019.lanecho.state.AppState

class LanechoApplication : Application() {

    lateinit var engine: Engine
        private set

    lateinit var appState: AppState
        private set

    override fun onCreate() {
        super.onCreate()
        Log.sink = { level, message ->
            when (level) {
                "warn" -> android.util.Log.w("lanecho", message)
                else -> android.util.Log.i("lanecho", message)
            }
        }
        engine = Engine(
            dataDir = filesDir.toPath(),
            imageCodec = AndroidImageCodec,
            defaultName = Build.MODEL ?: "Android",
            osVersion = "Android ${Build.VERSION.RELEASE}",
        )
        engine.initialize()
        appState = AppState(this, engine)
    }
}
