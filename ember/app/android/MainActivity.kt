// Ember's Android host (FR-L36, #770).
//
// compose-rust generates an Activity like this one for an application that has none. Ember
// keeps its own because three things are the platform's to say and the toolkit has no way to
// say them yet: where the data directory is, the launch settings of a debug run, and back
// (TODO(compose-rust#302): the toolkit has no BackRequested event). Everything else is the
// generated Activity's text for compose-rust v0.0.6: Kotlin owns the process and the frame
// loop, the interpreter draws the Host's tree in a ComposeView, and Rust runs as a cdylib in
// the same process. When the toolkit changes its generated Activity, this file follows it.
//
// Nothing here decides how a control looks or behaves (INTENT D25).
//
// The package and the library name are the packager's, not Ember's: dx puts the Activity in
// `dev.dioxus.main` and names the cdylib `libmain.so` whatever the application id is
// (`dev.darkpyonix.ember`, from Dioxus.toml).
package dev.dioxus.main

import android.content.Intent
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.system.Os
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.ComposeView
import dev.darkpyonix.composerust.runtime.ComposeRustContent
import dev.darkpyonix.composerust.ui.platform.AndroidNotifications
import dev.darkpyonix.composerust.ui.platform.ComposeRustRuntime
import dev.darkpyonix.composerust.ui.platform.installSystemChrome
import dev.darkpyonix.composerust.ui.platform.installZoomLevelStore

class MainActivity : ComponentActivity() {
    // Registered before the Activity starts, which is the only time the platform allows.
    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {
            AndroidNotifications.permissionAnswered()
        }

    // One back step in the app (ember/app/src/ui/back.rs). False: the app did not handle it.
    private external fun emberBack(): Boolean

    override fun onCreate(savedInstanceState: Bundle?) {
        installSystemChrome(this)
        installZoomLevelStore(this)
        super.onCreate(savedInstanceState)
        // The Host starts inside the library load below and lives as long as the process, so
        // its settings are in the environment before the first load and are not read again.
        if (!ComposeRustRuntime.isStarted) {
            exportSettings()
        }
        ComposeRustRuntime.load("main")
        AndroidNotifications.install(this) {
            notificationPermission.launch("android.permission.POST_NOTIFICATIONS")
        }
        AndroidNotifications.handle(intent)
        val host = ComposeRustRuntime.host()
        ComposeRustRuntime.attached()
        val view = ComposeView(this)
        view.setContent {
            // The whole window: the renderer keeps the page off the system bars itself.
            ComposeRustContent(host, Modifier.fillMaxSize())
        }
        setContentView(view)
        // The system back gesture and button are the app's back. A widget that goes back on
        // its own (the toolkit's split pane) registers later and so is asked first.
        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    if (!emberBack()) {
                        // Not handled: the system's own back, which leaves the app.
                        isEnabled = false
                        onBackPressedDispatcher.onBackPressed()
                        isEnabled = true
                    }
                }
            },
        )
    }

    // The settings the desktop reads from its environment, under the same names.
    private fun exportSettings() {
        // Cache, prefs and exports live in the application's own files directory.
        Os.setenv("EMBER_DATA_DIR", filesDir.absolutePath, true)
        // Launch settings come from the intent in a debuggable build only, so that another
        // application cannot point a released Ember at a server of its choosing.
        val debuggable = (applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0
        if (!debuggable) return
        for (name in arrayOf("EMBER_DEMO_DATA", "EMBER_SERVER_URL", "EMBER_CR_SHELL", "EMBER_CR_ROUTE")) {
            intent.getStringExtra(name)?.let { Os.setenv(name, it, true) }
        }
    }

    override fun onStart() {
        super.onStart()
        ComposeRustRuntime.start()
    }

    override fun onStop() {
        ComposeRustRuntime.stop()
        super.onStop()
    }

    // A press on a notification while this Activity is already running.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        AndroidNotifications.handle(intent)
    }

    override fun onDestroy() {
        // The process may stay without an Activity, and a worker's notification still has to
        // go out from it.
        ComposeRustRuntime.detached()
        super.onDestroy()
    }
}
