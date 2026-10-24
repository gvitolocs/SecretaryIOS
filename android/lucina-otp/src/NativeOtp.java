package tech.getsecretary.lucinaotp;
final class NativeOtp {
 static { System.loadLibrary("lucina_device"); }
 static native String parse(String sender,String body);
}
