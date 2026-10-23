// Unpack-Cleanup: removes ONLY Unpack's own per-user leftovers. No admin. Asks first.
#define UNICODE
#define _UNICODE
#include <windows.h>
#include <shlwapi.h>
#include <shlobj.h>
#include <stdio.h>
#include <wchar.h>

#define MAXI 128
typedef struct { int kind; wchar_t a[MAX_PATH*2]; wchar_t b[MAX_PATH]; wchar_t c[MAX_PATH]; } Item;
// kind: 1=delete HKCU key tree, 2=restore/clear .ext default (a=ext key, b=class, c=backup value),
//       3=delete folder tree, 4=delete file, 5=delete HKCU value (a=key,b=value), 6=delete key if empty
static Item it[MAXI]; static int n=0;
static wchar_t report[32768];

static int keyExists(const wchar_t*k){HKEY h; if(RegOpenKeyExW(HKEY_CURRENT_USER,k,0,KEY_READ,&h)==ERROR_SUCCESS){RegCloseKey(h);return 1;}return 0;}
static int readStr(const wchar_t*k,const wchar_t*v,wchar_t*out,DWORD cb){DWORD t; out[0]=0; return RegGetValueW(HKEY_CURRENT_USER,k,v,RRF_RT_REG_SZ|RRF_RT_REG_EXPAND_SZ|RRF_NOEXPAND,&t,out,&cb)==ERROR_SUCCESS;}
static void add(int kind,const wchar_t*a,const wchar_t*b,const wchar_t*c){ if(n>=MAXI)return; it[n].kind=kind; wcsncpy(it[n].a,a,MAX_PATH*2-1); wcsncpy(it[n].b,b?b:L"",MAX_PATH-1); wcsncpy(it[n].c,c?c:L"",MAX_PATH-1); n++; }
static int isOurs(const wchar_t*cmd){ return StrStrIW(cmd,L"\\unpack.exe")!=NULL; }

static void scan(void){
  const wchar_t*ext[]={L".zip",L".7z",L".rar",L".tar",L".gz",L".tgz",L".bz2",L".xz"};
  const wchar_t*verb[]={L"Unpack.ExtractHere",L"Unpack.ExtractFolder",L"Unpack.Open"};
  wchar_t k[MAX_PATH*2], v[2048];
  for(int i=0;i<8;i++){ for(int j=0;j<3;j++){
      swprintf(k,MAX_PATH*2,L"Software\\Classes\\SystemFileAssociations\\%ls\\shell\\%ls",ext[i],verb[j]);
      if(keyExists(k)) add(1,k,0,0);} }
  // Our file classes: only if their open command really points to unpack.exe
  const wchar_t*cls[][2]={{L".7z",L"7-Zip archive"},{L".rar",L"RAR archive"}};
  for(int i=0;i<2;i++){
    swprintf(k,MAX_PATH*2,L"Software\\Classes\\%ls\\shell\\open\\command",cls[i][1]);
    int ours = readStr(k,NULL,v,sizeof v) && isOurs(v);
    if(ours){ swprintf(k,MAX_PATH*2,L"Software\\Classes\\%ls",cls[i][1]); add(1,k,0,0); }
    swprintf(k,MAX_PATH*2,L"Software\\Classes\\%ls",cls[i][0]);
    wchar_t bk[MAX_PATH]; swprintf(bk,MAX_PATH,L"%ls_backup",cls[i][1]);
    wchar_t cur[512]; int hasCur=readStr(k,NULL,cur,sizeof cur);
    wchar_t tmp[512]; int hasBak=readStr(k,bk,tmp,sizeof tmp);
    if((hasCur && (wcscmp(cur,cls[i][1])==0 || (cur[0]==0 && hasBak))) || hasBak) add(2,k,cls[i][1],bk);
  }
  // Uninstall entry + install-location key, only if the real uninstaller is gone (leftover)
  const wchar_t*un=L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Unpack";
  if(readStr(un,L"UninstallString",v,sizeof v) && StrStrIW(v,L"uninstall.exe")){
    wchar_t p[2048]; wcsncpy(p,v,2047); PathUnquoteSpacesW(p);
    if(GetFileAttributesW(p)==INVALID_FILE_ATTRIBUTES){ add(1,un,0,0); }
  }
  // Install-location key and app data: current identifier (com.qeezyxz.unpack) and the pre-1.6 one (com.justin.unpack)
  static const wchar_t*mks[][2]={{L"Software\\qeezyxz\\Unpack",L"Software\\qeezyxz"},{L"Software\\justin\\Unpack",L"Software\\justin"}};
  for(int i=0;i<2;i++){ const wchar_t*mk=mks[i][0];
    if(keyExists(mk)){ wchar_t dir[2048]; readStr(mk,NULL,dir,sizeof dir);
      wchar_t ex[2100]; swprintf(ex,2100,L"%ls\\uninstall.exe",dir);
      if(dir[0]==0 || GetFileAttributesW(ex)==INVALID_FILE_ATTRIBUTES){ add(1,mk,0,0); add(6,mks[i][1],0,0);} } }
  // App data (WebView2 cache, settings, history)
  wchar_t e[MAX_PATH], p[MAX_PATH*2];
  static const wchar_t*ids[]={L"com.qeezyxz.unpack",L"com.justin.unpack"};
  for(int i=0;i<2;i++){
    if(GetEnvironmentVariableW(L"LOCALAPPDATA",e,MAX_PATH)){ swprintf(p,MAX_PATH*2,L"%ls\\%ls",e,ids[i]); if(GetFileAttributesW(p)!=INVALID_FILE_ATTRIBUTES) add(3,p,0,0);}
    if(GetEnvironmentVariableW(L"APPDATA",e,MAX_PATH)){ swprintf(p,MAX_PATH*2,L"%ls\\%ls",e,ids[i]); if(GetFileAttributesW(p)!=INVALID_FILE_ATTRIBUTES) add(3,p,0,0);}
  }
  // Temp files the app makes: %TEMP%\unpack-entry-*.txt
  wchar_t t[MAX_PATH]; if(GetTempPathW(MAX_PATH,t)){ WIN32_FIND_DATAW fd; swprintf(p,MAX_PATH*2,L"%lsunpack-entry-*.txt",t);
    HANDLE h=FindFirstFileW(p,&fd); if(h!=INVALID_HANDLE_VALUE){ do{ if(!(fd.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY)){ wchar_t f[MAX_PATH*2]; swprintf(f,MAX_PATH*2,L"%ls%ls",t,fd.cFileName); add(4,f,0,0);} }while(FindNextFileW(h,&fd)); FindClose(h);} }
}

static int quiet=0;
static void say(const wchar_t*s,UINT f){ if(quiet) fwprintf(stdout,L"%ls\n",s); else MessageBoxW(NULL,s,L"Unpack Cleanup",f); }

int WINAPI wWinMain(HINSTANCE hi,HINSTANCE hp,PWSTR cmd,int show){
  int yes = cmd && StrStrIW(cmd,L"--yes")!=NULL;  // for testing: skip the prompt
  quiet = cmd && StrStrIW(cmd,L"--list")!=NULL;
  scan();
  if(n==0){ say(L"Nothing to clean: no Unpack leftovers were found for this user.",MB_ICONINFORMATION); return 0; }
  wcscpy(report,L"Unpack Cleanup will remove ONLY these Unpack leftovers (current user, no admin):\n\n");
  for(int i=0;i<n;i++){ wchar_t line[MAX_PATH*3];
    switch(it[i].kind){
      case 1: swprintf(line,MAX_PATH*3,L"  Registry key: HKCU\\%ls\n",it[i].a); break;
      case 2: swprintf(line,MAX_PATH*3,L"  Restore previous handler: HKCU\\%ls\n",it[i].a); break;
      case 3: swprintf(line,MAX_PATH*3,L"  Folder: %ls\n",it[i].a); break;
      case 4: swprintf(line,MAX_PATH*3,L"  Temp file: %ls\n",it[i].a); break;
      default: line[0]=0; }
    if(wcslen(report)+wcslen(line)<32000) wcscat(report,line); }
  wcscat(report,L"\nNothing else on your PC is touched. Continue?");
  if(quiet){ fwprintf(stdout,L"%ls\n",report); if(!yes) return 0; }
  else if(!yes && MessageBoxW(NULL,report,L"Unpack Cleanup",MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2)!=IDYES) return 0;
  int fails=0;
  for(int i=0;i<n;i++){ LSTATUS r=0;
    switch(it[i].kind){
      case 1: r=RegDeleteTreeW(HKEY_CURRENT_USER,it[i].a); if(r==ERROR_SUCCESS) RegDeleteKeyW(HKEY_CURRENT_USER,it[i].a); r=keyExists(it[i].a); break;
      case 2: { HKEY h; if(RegOpenKeyExW(HKEY_CURRENT_USER,it[i].a,0,KEY_READ|KEY_SET_VALUE,&h)==ERROR_SUCCESS){
          wchar_t cur[512],bak[512]; readStr(it[i].a,NULL,cur,sizeof cur); int hb=readStr(it[i].a,it[i].c,bak,sizeof bak);
          if(wcscmp(cur,it[i].b)==0 || cur[0]==0){
            if(hb && bak[0] && wcscmp(bak,it[i].b)!=0) RegSetValueExW(h,NULL,0,REG_SZ,(BYTE*)bak,(DWORD)(wcslen(bak)+1)*2);
            else RegDeleteValueW(h,NULL); }
          RegDeleteValueW(h,it[i].c); RegCloseKey(h);
          DWORD sk=0,vc=0; if(RegOpenKeyExW(HKEY_CURRENT_USER,it[i].a,0,KEY_READ,&h)==ERROR_SUCCESS){ RegQueryInfoKeyW(h,0,0,0,&sk,0,0,&vc,0,0,0,0); RegCloseKey(h); if(sk==0&&vc==0) RegDeleteKeyW(HKEY_CURRENT_USER,it[i].a);} } break; }
      case 3: { wchar_t from[MAX_PATH*2+2]={0}; wcsncpy(from,it[i].a,MAX_PATH*2-1); from[wcslen(from)+1]=0;
          SHFILEOPSTRUCTW op={0}; op.wFunc=FO_DELETE; op.pFrom=from; op.fFlags=FOF_NOCONFIRMATION|FOF_NOERRORUI|FOF_SILENT;
          SHFileOperationW(&op); r=GetFileAttributesW(it[i].a)!=INVALID_FILE_ATTRIBUTES; break; }
      case 4: r=!DeleteFileW(it[i].a); break;
      case 6: RegDeleteKeyW(HKEY_CURRENT_USER,it[i].a); r=0; break;  // fails harmlessly if not empty
    }
    if(r) fails++; }
  // remove now-empty SystemFileAssociations\.ext\shell keys we emptied
  const wchar_t*ext[]={L".zip",L".7z",L".rar",L".tar",L".gz",L".tgz",L".bz2",L".xz"};
  for(int i=0;i<8;i++){ wchar_t k[MAX_PATH]; swprintf(k,MAX_PATH,L"Software\\Classes\\SystemFileAssociations\\%ls\\shell",ext[i]);
    RegDeleteKeyW(HKEY_CURRENT_USER,k); swprintf(k,MAX_PATH,L"Software\\Classes\\SystemFileAssociations\\%ls",ext[i]); RegDeleteKeyW(HKEY_CURRENT_USER,k); }
  SHChangeNotify(SHCNE_ASSOCCHANGED,SHCNF_IDLIST,NULL,NULL);
  say(fails? L"Done, but some items couldn't be removed (maybe Unpack is still running). Close it and run Cleanup again."
           : L"Done. All listed Unpack leftovers were removed.", fails?MB_ICONWARNING:MB_ICONINFORMATION);
  return fails?1:0;
}
