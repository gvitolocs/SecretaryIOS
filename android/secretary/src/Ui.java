package tech.getsecretary.secretary;
import android.app.*;import android.graphics.Color;import android.view.*;import android.widget.*;
class Ui extends Activity {
 LinearLayout body;final int NAVY=Color.rgb(10,14,32),CYAN=Color.rgb(80,223,255);
 void screen(String title){ScrollView scroll=new ScrollView(this);scroll.setBackgroundColor(NAVY);body=new LinearLayout(this);body.setOrientation(1);body.setPadding(28,38,28,40);scroll.addView(body);setContentView(scroll);TextView h=text(title,28);h.setTextColor(CYAN);}
 TextView text(String value,int size){TextView t=new TextView(this);t.setText(value);t.setTextSize(size);t.setTextColor(Color.rgb(224,231,245));t.setPadding(0,12,0,12);body.addView(t);return t;}
 Button button(String title,Runnable action){Button b=new Button(this);b.setText(title);b.setAllCaps(false);body.addView(b);b.setOnClickListener(v->action.run());return b;}
 void notice(String text){Toast.makeText(this,text,Toast.LENGTH_LONG).show();}
}
