# R8 rules for Lucina OTP. Manifest components come from aapt2 --proguard (build/aapt-rules.pro).

# JNI: liblucina_android.so binds Java_tech_getsecretary_lucinaotp_Native_<name>.
-keep,includedescriptorclasses class tech.getsecretary.lucinaotp.Native {
    native <methods>;
}

# The Kotlin null-check intrinsics only build exception messages; drop them.
-assumenosideeffects class kotlin.jvm.internal.Intrinsics {
    static void checkNotNull(...);
    static void checkNotNullParameter(...);
    static void checkNotNullExpressionValue(...);
    static void checkParameterIsNotNull(...);
    static void checkExpressionValueIsNotNull(...);
    static void checkReturnedValueIsNotNull(...);
    static void checkFieldIsNotNull(...);
}

-allowaccessmodification
-repackageclasses ''
-dontwarn kotlin.**
-dontwarn org.jetbrains.annotations.**
