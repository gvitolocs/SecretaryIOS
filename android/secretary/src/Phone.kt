package tech.getsecretary.secretary

import android.app.role.RoleManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.telecom.TelecomManager
import android.telephony.PhoneNumberUtils
import android.telephony.TelephonyManager

/**
 * Android's phone plumbing for the dialer role, kept free of Rust and the network so calls (above all
 * emergency calls) never depend on them. Numbers are routed by [DialPlan].
 */
object Phone {
    /** Samsung Phone on One UI (Android 13); the real package is remembered from the device when it is seen. */
    const val SAMSUNG_DIALER = "com.samsung.android.dialer"
    private const val PREF = "phone"
    private const val PREVIOUS = "previous_dialer"

    /** `TelecomManager.placeCall(tel:<number>, Bundle())`, the system path for every call. */
    fun placer(c: Context): DialPlan.Placer = DialPlan.Placer { scheme, number ->
        c.getSystemService(TelecomManager::class.java)
            .placeCall(Uri.fromParts(scheme, number, null), Bundle())
    }

    /**
     * `TelephonyManager.isEmergencyNumber`, then the framework's own `PhoneNumberUtils` table if that needs a
     * permission or fails. 112 and 911 always count: they are emergency numbers on every network.
     */
    @Suppress("DEPRECATION")
    fun isEmergency(c: Context, n: String): Boolean {
        if (n == "112" || n == "911") return true
        try {
            if (c.getSystemService(TelephonyManager::class.java).isEmergencyNumber(n)) return true
        } catch (t: Throwable) {
        }
        return try {
            PhoneNumberUtils.isEmergencyNumber(n)
        } catch (t: Throwable) {
            false
        }
    }

    /** Dials [raw]. An emergency number never goes anywhere but Telecom. */
    fun dial(c: Context, raw: String): DialPlan.Outcome {
        val out = DialPlan.dial(raw, { isEmergency(c, it) }, placer(c))
        if (out is DialPlan.Outcome.Failed && out.emergency) emergencyFallback(c, out.number)
        return out
    }

    /** Telecom refused an emergency call (no CALL_PHONE and not the phone app): open a system dialer with the number ready. */
    private fun emergencyFallback(c: Context, number: String) {
        val telecom = c.getSystemService(TelecomManager::class.java)
        val target = (telecom.systemDialerPackage ?: telecom.defaultDialerPackage)?.takeIf { it != c.packageName }
        val intent = Intent(Intent.ACTION_DIAL, Uri.fromParts("tel", number, null)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            c.startActivity(if (target != null) intent.setPackage(target) else intent)
        } catch (t: Throwable) {
            try {
                c.startActivity(intent.setPackage(null))
            } catch (u: Throwable) {
            }
        }
    }

    fun callVoicemail(c: Context): Boolean = DialPlan.voicemail(placer(c))

    // ---- the default phone app

    fun isDefaultDialer(c: Context): Boolean =
        try {
            c.getSystemService(TelecomManager::class.java).defaultDialerPackage == c.packageName
        } catch (t: Throwable) {
            false
        }

    fun roleAvailable(c: Context): Boolean = c.getSystemService(RoleManager::class.java).isRoleAvailable(RoleManager.ROLE_DIALER)

    fun roleHeld(c: Context): Boolean = roleAvailable(c) && c.getSystemService(RoleManager::class.java).isRoleHeld(RoleManager.ROLE_DIALER)

    /** The intent that asks the owner to make this app the phone app; he does the final tap. */
    fun requestRole(c: Context): Intent? {
        val roles = c.getSystemService(RoleManager::class.java)
        if (!roles.isRoleAvailable(RoleManager.ROLE_DIALER) || roles.isRoleHeld(RoleManager.ROLE_DIALER)) return null
        rememberPrevious(c)
        return roles.createRequestRoleIntent(RoleManager.ROLE_DIALER)
    }

    /** Remembers which phone app was the default before this one, to offer the way back. */
    fun rememberPrevious(c: Context) {
        try {
            val pkg = c.getSystemService(TelecomManager::class.java).defaultDialerPackage
            if (pkg != null && pkg != c.packageName) {
                c.getSharedPreferences(PREF, 0).edit().putString(PREVIOUS, pkg).apply()
            }
        } catch (t: Throwable) {
        }
    }

    private fun installed(c: Context, pkg: String) = try {
        c.packageManager.getPackageInfo(pkg, 0)
        true
    } catch (e: PackageManager.NameNotFoundException) {
        false
    }

    /** The phone app to go back to: the one that was default before, else Samsung Phone. */
    fun previousDialer(c: Context): String? {
        val saved = c.getSharedPreferences(PREF, 0).getString(PREVIOUS, null)
        return listOfNotNull(saved, SAMSUNG_DIALER, "com.google.android.dialer").firstOrNull { it != c.packageName && installed(c, it) }
    }

    /**
     * "Use Samsung Phone again": Telecom's dialog that offers the old phone app as the default, else the
     * system's Default apps screen. Only the owner's tap there changes anything. Returns the request code
     * used for a result, or -1 when the settings screen was opened.
     */
    @Suppress("DEPRECATION")
    fun switchBack(a: android.app.Activity, request: Int): Int {
        val pkg = previousDialer(a)
        if (pkg != null) {
            try {
                a.startActivityForResult(
                    Intent(TelecomManager.ACTION_CHANGE_DEFAULT_DIALER).putExtra(TelecomManager.EXTRA_CHANGE_DEFAULT_DIALER_PACKAGE_NAME, pkg),
                    request,
                )
                return request
            } catch (e: ActivityNotFoundException) {
            } catch (e: SecurityException) {
            }
        }
        for (action in listOf(Settings.ACTION_MANAGE_DEFAULT_APPS_SETTINGS, Settings.ACTION_SETTINGS)) {
            try {
                a.startActivity(Intent(action))
                break
            } catch (e: ActivityNotFoundException) {
            }
        }
        return -1
    }
}
