#include "BwSdkModule.h"

namespace bw
{
    SdkComponent::SdkComponent(IServer& server)
        : server_(server)
    {
    }

    bool SdkComponent::Login(const std::string& user, const std::string& password)
    {
        if (logged_in_)
        {
            return false;
        }

        if (!server_.Login(user, password))
        {
            return false;
        }

        logged_in_ = true;
        return true;
    }

    void SdkComponent::Logout()
    {
        if (!logged_in_)
        {
            return;
        }

        server_.Logout();
        logged_in_ = false;
    }

    RpcResult SdkComponent::Rpc(long long entityId,
                                const std::string& method,
                                const std::vector<std::string>& parameters)
    {
        RpcResult result;
        if (!logged_in_)
        {
            return result;
        }

        std::string answer;
        result.answered = server_.Rpc(entityId, method, parameters, answer);
        result.answer = answer;
        return result;
    }

    bool SdkComponent::GetProperty(long long entityId, const std::string& path, std::string& value)
    {
        return server_.GetProperty(entityId, path, value);
    }
}
