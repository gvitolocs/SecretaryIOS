package tech.getsecretary.secretary;
import android.app.*;import android.os.*;import android.content.*;import android.net.Uri;import android.webkit.*;import android.widget.*;import android.graphics.Color;
public final class MainActivity extends Activity {
 WebView web;
 public void onCreate(Bundle b){super.onCreate(b);LinearLayout root=new LinearLayout(this);root.setOrientation(1);root.setBackgroundColor(Color.rgb(10,14,32));setContentView(root);
 LinearLayout nav=new LinearLayout(this);root.addView(nav);
 for(String name:new String[]{"Lucina","My desk","Calls"}){Button button=new Button(this);button.setText(name);button.setAllCaps(false);nav.addView(button,new LinearLayout.LayoutParams(0,-2,1));button.setOnClickListener(v->{if(name.equals("Calls"))startActivity(new Intent(this,CallsActivity.class));else web.loadUrl("https://getsecretary.tech"+(name.equals("My desk")?"/desk":"/"));});}
 web=new WebView(this);root.addView(web,new LinearLayout.LayoutParams(-1,0,1));web.getSettings().setJavaScriptEnabled(true);web.getSettings().setDomStorageEnabled(true);web.getSettings().setAllowFileAccess(false);web.getSettings().setAllowContentAccess(false);web.getSettings().setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);CookieManager.getInstance().setAcceptCookie(true);CookieManager.getInstance().setAcceptThirdPartyCookies(web,false);
 web.setWebViewClient(new WebViewClient(){public boolean shouldOverrideUrlLoading(WebView view,WebResourceRequest request){Uri uri=request.getUrl();String host=uri.getHost();if("https".equals(uri.getScheme())&&"getsecretary.tech".equals(host))return false;if("tel".equals(uri.getScheme()))startActivity(new Intent(MainActivity.this,CallsActivity.class).setData(uri));else if("https".equals(uri.getScheme()))startActivity(new Intent(Intent.ACTION_VIEW,uri));return true;}});
 web.loadUrl(b==null?"https://getsecretary.tech/":b.getString("url","https://getsecretary.tech/"));
 }
 public void onSaveInstanceState(Bundle b){super.onSaveInstanceState(b);b.putString("url",web.getUrl());}
 public void onBackPressed(){if(web.canGoBack())web.goBack();else super.onBackPressed();}
}
