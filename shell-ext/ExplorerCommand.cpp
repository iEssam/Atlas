#include <windows.h>
#include <shobjidl.h>

#include <atomic>
#include <cstring>
#include <new>
#include <string>
#include <vector>

namespace
{
constexpr CLSID CommandClsid = { 0x2c9e70c5, 0x5c34, 0x4d34, { 0x98, 0x4a, 0x59, 0x56, 0xb8, 0xd0, 0xe1, 0x1d } };
HINSTANCE moduleInstance = nullptr;
std::atomic<long> moduleLocks = 0;

HRESULT CopyString(const std::wstring& value, PWSTR* result) noexcept
{
    if (result == nullptr) return E_POINTER;
    *result = nullptr;
    const auto bytes = (value.size() + 1) * sizeof(wchar_t);
    auto copy = static_cast<PWSTR>(CoTaskMemAlloc(bytes));
    if (copy == nullptr) return E_OUTOFMEMORY;
    std::memcpy(copy, value.c_str(), bytes);
    *result = copy;
    return S_OK;
}

HRESULT GetSelectedFile(IShellItemArray* items, std::wstring& path) noexcept
{
    if (items == nullptr) return E_INVALIDARG;
    DWORD count = 0;
    HRESULT hr = items->GetCount(&count);
    if (FAILED(hr) || count != 1) return E_INVALIDARG;
    IShellItem* item = nullptr;
    hr = items->GetItemAt(0, &item);
    if (FAILED(hr)) return hr;
    SFGAOF attributes = 0;
    hr = item->GetAttributes(SFGAO_FOLDER, &attributes);
    if (SUCCEEDED(hr) && (attributes & SFGAO_FOLDER) != 0)
    {
        item->Release();
        return E_INVALIDARG;
    }
    PWSTR rawPath = nullptr;
    hr = item->GetDisplayName(SIGDN_FILESYSPATH, &rawPath);
    item->Release();
    if (FAILED(hr)) return hr;
    path.assign(rawPath);
    CoTaskMemFree(rawPath);
    return path.empty() ? E_INVALIDARG : S_OK;
}

HRESULT GetApplicationPath(std::wstring& appPath, std::wstring& directory) noexcept
{
    std::vector<wchar_t> buffer(32768);
    const DWORD length = GetModuleFileNameW(moduleInstance, buffer.data(), static_cast<DWORD>(buffer.size()));
    if (length == 0) return HRESULT_FROM_WIN32(GetLastError());
    if (length >= buffer.size()) return HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER);
    directory.assign(buffer.data(), length);
    const auto separator = directory.find_last_of(L"\\/");
    if (separator == std::wstring::npos) return E_UNEXPECTED;
    directory.resize(separator);
    appPath = directory + L"\\Atlas.App.exe";
    return S_OK;
}

class ExplorerCommand final : public IExplorerCommand
{
public:
    ExplorerCommand() noexcept { ++moduleLocks; }
    ~ExplorerCommand() { --moduleLocks; }
    IFACEMETHODIMP QueryInterface(REFIID iid, void** object) override
    {
        if (object == nullptr) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_IExplorerCommand) { *object = static_cast<IExplorerCommand*>(this); AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    IFACEMETHODIMP_(ULONG) AddRef() override { return static_cast<ULONG>(++references_); }
    IFACEMETHODIMP_(ULONG) Release() override { const auto remaining = --references_; if (remaining == 0) delete this; return static_cast<ULONG>(remaining); }
    IFACEMETHODIMP GetTitle(IShellItemArray*, PWSTR* title) override { return CopyString(L"Find what is using this file", title); }
    IFACEMETHODIMP GetIcon(IShellItemArray*, PWSTR* icon) override
    {
        std::wstring appPath, directory;
        const HRESULT hr = GetApplicationPath(appPath, directory);
        return FAILED(hr) ? hr : CopyString(appPath + L",0", icon);
    }
    IFACEMETHODIMP GetToolTip(IShellItemArray*, PWSTR* tip) override { return CopyString(L"Show the processes currently holding this file open", tip); }
    IFACEMETHODIMP GetCanonicalName(GUID* guid) override { if (guid == nullptr) return E_POINTER; *guid = CommandClsid; return S_OK; }
    IFACEMETHODIMP GetState(IShellItemArray* items, BOOL, EXPCMDSTATE* state) override
    {
        if (state == nullptr) return E_POINTER;
        std::wstring path;
        *state = SUCCEEDED(GetSelectedFile(items, path)) ? ECS_ENABLED : ECS_HIDDEN;
        return S_OK;
    }
    IFACEMETHODIMP Invoke(IShellItemArray* items, IBindCtx*) override
    {
        std::wstring filePath;
        HRESULT hr = GetSelectedFile(items, filePath);
        if (FAILED(hr)) return hr;
        std::wstring appPath, directory;
        hr = GetApplicationPath(appPath, directory);
        if (FAILED(hr)) return hr;
        std::wstring commandLine = L"\"" + appPath + L"\" --find-using \"" + filePath + L"\"";
        std::vector<wchar_t> mutableCommand(commandLine.begin(), commandLine.end());
        mutableCommand.push_back(L'\0');
        STARTUPINFOW startup{ sizeof(startup) };
        PROCESS_INFORMATION process{};
        if (!CreateProcessW(appPath.c_str(), mutableCommand.data(), nullptr, nullptr, FALSE, CREATE_UNICODE_ENVIRONMENT, nullptr, directory.c_str(), &startup, &process)) return HRESULT_FROM_WIN32(GetLastError());
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
        return S_OK;
    }
    IFACEMETHODIMP GetFlags(EXPCMDFLAGS* flags) override { if (flags == nullptr) return E_POINTER; *flags = ECF_DEFAULT; return S_OK; }
    IFACEMETHODIMP EnumSubCommands(IEnumExplorerCommand** commands) override { if (commands != nullptr) *commands = nullptr; return E_NOTIMPL; }
private:
    std::atomic<ULONG> references_ = 1;
};

class CommandFactory final : public IClassFactory
{
public:
    CommandFactory() noexcept { ++moduleLocks; }
    ~CommandFactory() { --moduleLocks; }
    IFACEMETHODIMP QueryInterface(REFIID iid, void** object) override
    {
        if (object == nullptr) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_IClassFactory) { *object = static_cast<IClassFactory*>(this); AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    IFACEMETHODIMP_(ULONG) AddRef() override { return static_cast<ULONG>(++references_); }
    IFACEMETHODIMP_(ULONG) Release() override { const auto remaining = --references_; if (remaining == 0) delete this; return static_cast<ULONG>(remaining); }
    IFACEMETHODIMP CreateInstance(IUnknown* outer, REFIID iid, void** object) override
    {
        if (outer != nullptr) return CLASS_E_NOAGGREGATION;
        auto command = new (std::nothrow) ExplorerCommand();
        if (command == nullptr) return E_OUTOFMEMORY;
        const HRESULT hr = command->QueryInterface(iid, object);
        command->Release();
        return hr;
    }
    IFACEMETHODIMP LockServer(BOOL lock) override { if (lock) ++moduleLocks; else --moduleLocks; return S_OK; }
private:
    std::atomic<ULONG> references_ = 1;
};
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, void*)
{
    if (reason == DLL_PROCESS_ATTACH) { moduleInstance = instance; DisableThreadLibraryCalls(instance); }
    return TRUE;
}

STDAPI DllCanUnloadNow() { return moduleLocks == 0 ? S_OK : S_FALSE; }
STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** object)
{
    if (clsid != CommandClsid) return CLASS_E_CLASSNOTAVAILABLE;
    auto factory = new (std::nothrow) CommandFactory();
    if (factory == nullptr) return E_OUTOFMEMORY;
    const HRESULT hr = factory->QueryInterface(iid, object);
    factory->Release();
    return hr;
}
