package tech.getsecretary.secretary

import android.app.Person
import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon
import android.os.Build

/** Framework Direct Share publishing; chooser ranking remains Android's decision. */
object ShareShortcut {
    const val ID = "lucina"
    const val CATEGORY = "tech.getsecretary.secretary.category.SHARE_LUCINA"

    fun publish(c: Context, used: Boolean = false) {
        try {
            val manager = c.getSystemService(ShortcutManager::class.java)
            val icon = Icon.createWithResource(c, c.resources.getIdentifier("lucina", "drawable", c.packageName))
            val shortcut = ShortcutInfo.Builder(c, ID)
                .setShortLabel("Lucina")
                .setLongLived(true)
                .setIcon(icon)
                .setCategories(setOf(CATEGORY))
                .setPerson(Person.Builder().setName("Lucina").setKey(ID).setBot(true).build())
                .setIntent(Intent(c, MainActivity::class.java).setAction(Intent.ACTION_VIEW))
                .build()
            if (Build.VERSION.SDK_INT >= 30) manager.pushDynamicShortcut(shortcut)
            else manager.addDynamicShortcuts(listOf(shortcut))
            if (used) manager.reportShortcutUsed(ID)
        } catch (e: RuntimeException) {
            // A launcher may reject updates or throttle them; ordinary sharing still works.
        }
    }
}
