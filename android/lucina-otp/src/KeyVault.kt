package tech.getsecretary.lucinaotp

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.system.Os
import java.io.DataInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The 32-byte data key of the Rust state, wrapped by a non-exportable AES-256-GCM key in AndroidKeyStore.
 * File layout of filesDir/lucina/key.wrapped: 12-byte IV, then ciphertext + 16-byte tag.
 */
internal object KeyVault {
    private const val ALIAS = "lucina-otp-data"
    private const val STORE = "AndroidKeyStore"
    private const val CIPHER = "AES/GCM/NoPadding"
    private const val IV_LEN = 12

    /** Returns a fresh copy of the data key; the caller zeroes it. */
    fun dataKey(context: Context): ByteArray {
        val dir = File(context.filesDir, "lucina")
        if (!dir.isDirectory && !dir.mkdirs()) throw IllegalStateException("no state dir")
        Os.chmod(dir.path, 0x1c0) // 0700
        val file = File(dir, "key.wrapped")
        val store = KeyStore.getInstance(STORE).apply { load(null) }
        val wrapping = store.getKey(ALIAS, null) as SecretKey?
        if (wrapping != null && file.isFile) {
            val blob = ByteArray(IV_LEN + 32 + 16)
            if (file.length() == blob.size.toLong()) {
                DataInputStream(FileInputStream(file)).use { it.readFully(blob) }
                try {
                    val cipher = Cipher.getInstance(CIPHER)
                    cipher.init(Cipher.DECRYPT_MODE, wrapping, GCMParameterSpec(128, blob, 0, IV_LEN))
                    return cipher.doFinal(blob, IV_LEN, blob.size - IV_LEN)
                } catch (_: AEADBadTagException) {
                    // Wrapped under a key that no longer exists; start over below.
                } finally {
                    blob.fill(0)
                }
            }
        }
        // First run, or the Keystore key was lost: a new data key. Old encrypted state becomes unreadable.
        val key = wrapping ?: generate()
        val data = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance(CIPHER)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val iv = cipher.iv
        val sealed = cipher.doFinal(data)
        val tmp = File(dir, "key.wrapped.tmp")
        FileOutputStream(tmp).use { out ->
            out.write(iv)
            out.write(sealed)
            out.fd.sync()
        }
        Os.chmod(tmp.path, 0x180) // 0600
        if (!tmp.renameTo(file)) throw IllegalStateException("key write failed")
        return data
    }

    private fun generate(): SecretKey {
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, STORE)
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }
}
