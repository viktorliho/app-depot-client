// Freestanding Win64 helper. Keep the runtime's literal argument tail intact.
typedef unsigned long DWORD;
typedef unsigned short WCHAR;
typedef void *HANDLE;
typedef struct {
  DWORD size;
  WCHAR *reserved, *desktop, *title;
  DWORD dimensions[8];
  unsigned short show, reserved_size;
  void *reserved_data;
  HANDLE input, output, error;
} Startup;
typedef struct {
  HANDLE process, thread;
  DWORD pid, tid;
} Process;
typedef struct {
  long long times[4];
  DWORD faults, total, active, terminated;
} Accounting;
// These structures mirror the stable Win64 ABI without a cross-platform SDK.
_Static_assert(sizeof(Startup) == 104, "STARTUPINFOW");
_Static_assert(sizeof(Process) == 24, "PROCESS_INFORMATION");
_Static_assert(sizeof(Accounting) == 48,
               "JOBOBJECT_BASIC_ACCOUNTING_INFORMATION");
enum {
  CreateSuspended = 4,
  BasicAccounting = 1,
  InstallerCancelled = 1602,
  GenericWrite = 0x40000000,
  CreateNew = 1,
  NormalFile = 0x80
};
#define API __declspec(dllimport)
API WCHAR *GetCommandLineW(void);
API DWORD GetEnvironmentVariableW(const WCHAR *, WCHAR *, DWORD);
API HANDLE CreateJobObjectW(void *, void *);
API int CreateProcessW(WCHAR *, WCHAR *, void *, void *, int, DWORD, void *,
                       void *, Startup *, Process *);
API int AssignProcessToJobObject(HANDLE, HANDLE);
API DWORD ResumeThread(HANDLE);
API int TerminateProcess(HANDLE, DWORD);
API DWORD WaitForSingleObject(HANDLE, DWORD);
API int GetExitCodeProcess(HANDLE, DWORD *);
API int QueryInformationJobObject(HANDLE, int, void *, DWORD, DWORD *);
API void Sleep(DWORD);
API DWORD GetFileAttributesW(const WCHAR *);
API int TerminateJobObject(HANDLE, DWORD);
API HANDLE CreateFileW(WCHAR *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API int WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API int FlushFileBuffers(HANDLE);
API void ExitProcess(DWORD);
static Startup startup;
static Process process;
static WCHAR target[32768], result[32768], cancel[32768];
void entry(void) {
  WCHAR *tail = GetCommandLineW();
  int quoted = 0;
  while (*tail && (quoted || (*tail != ' ' && *tail != 9))) {
    if (*tail == '"')
      quoted = !quoted;
    ++tail;
  }
  while (*tail == ' ' || *tail == 9)
    ++tail;
  if (!*tail || !GetEnvironmentVariableW(L"DEPOT_INSTALLER", target, 32768) ||
      !GetEnvironmentVariableW(L"DEPOT_RESULT", result, 32768) ||
      !GetEnvironmentVariableW(L"DEPOT_CANCEL", cancel, 32768))
    ExitProcess(1);
  HANDLE job = CreateJobObjectW(0, 0);
  startup.size = sizeof(startup);
  if (!job || !CreateProcessW(target, tail, 0, 0, 0, CreateSuspended, 0, 0,
                              &startup, &process))
    ExitProcess(2);
  if (!AssignProcessToJobObject(job, process.process) ||
      ResumeThread(process.thread) == 0xffffffff) {
    TerminateProcess(process.process, 1);
    ExitProcess(3);
  }
  DWORD code, written;
  int cancelled = 0;
  Accounting accounting;
  for (;;) {
    if (!QueryInformationJobObject(job, BasicAccounting, &accounting,
                                   sizeof(accounting), 0))
      ExitProcess(5);
    if (!accounting.active)
      break;
    if (!cancelled && GetFileAttributesW(cancel) != 0xffffffff) {
      if (!TerminateJobObject(job, InstallerCancelled))
        ExitProcess(7);
      cancelled = 1;
    }
    Sleep(100);
  }
  if (WaitForSingleObject(process.process, 0xffffffff) ||
      !GetExitCodeProcess(process.process, &code))
    ExitProcess(4);
  if (cancelled)
    code = InstallerCancelled;
  HANDLE file =
      CreateFileW(result, GenericWrite, 0, 0, CreateNew, NormalFile, 0);
  if (file == (HANDLE)-1 || !WriteFile(file, &code, 4, &written, 0) ||
      written != 4 || !FlushFileBuffers(file))
    ExitProcess(6);
  ExitProcess(0);
}
