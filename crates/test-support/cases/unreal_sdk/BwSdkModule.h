// The Unreal adapter's C++ surface (BW-21-04-02).
//
// The adapter talks to a server through [IServer], so the shape is checkable
// without a live server, and the example logs in and issues an RPC against a
// scripted stand-in.

#pragma once

#include <string>
#include <vector>

namespace bw
{
    /// One RPC round trip the example reports.
    struct RpcResult
    {
        bool answered = false;
        std::string answer;
    };

    /// A server the adapter talks to.  A test stands in for a live server
    /// with a scripted object.
    class IServer
    {
    public:
        virtual ~IServer() = default;

        virtual bool Login(const std::string& user, const std::string& password) = 0;
        virtual void Logout() = 0;
        virtual bool Rpc(long long entityId,
                         const std::string& method,
                         const std::vector<std::string>& parameters,
                         std::string& answer) = 0;
        virtual bool SyncProperty(long long entityId,
                                  const std::string& path,
                                  const std::string& value) = 0;
        virtual bool GetProperty(long long entityId,
                                 const std::string& path,
                                 std::string& value) = 0;
    };

    /// The SDK's Unreal adapter: the C++ view a component wraps.
    class SdkComponent
    {
    public:
        explicit SdkComponent(IServer& server);

        /// Log in; a refused login leaves the component logged out.
        bool Login(const std::string& user, const std::string& password);

        /// Log out; a logged-out component logs out again without a server
        /// call.
        void Logout();

        /// Make an RPC call and read its answer beside the parameters.
        RpcResult Rpc(long long entityId,
                      const std::string& method,
                      const std::vector<std::string>& parameters);

        /// Sync one property by its entity and path; a logged-out component
        /// refuses.
        bool SyncProperty(long long entityId, const std::string& path, const std::string& value);

        /// Read one property by its entity and path.
        bool GetProperty(long long entityId, const std::string& path, std::string& value);

        /// Whether the component is logged in.
        bool LoggedIn() const { return logged_in_; }

    private:
        IServer& server_;
        bool logged_in_ = false;
    };
}
