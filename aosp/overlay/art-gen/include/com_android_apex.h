// Reader for apexd's apex-info-list.xml, the subset of the XSD-generated
// C++ API ("apex-info-list-tinyxml" on Android) that libart uses
// (runtime.cc, the APEX versions of the boot class path). Written on top of
// tinyxml2 because Andruna has no xsdc.

#pragma once

#include <cstdint>
#include <cstdlib>
#include <optional>
#include <string>
#include <vector>

#include <tinyxml2.h>

namespace com {
namespace android {
namespace apex {

class ApexInfo {
 public:
  const std::string& getModuleName() const { return module_name_; }
  bool getIsActive() const { return is_active_; }
  bool getIsFactory() const { return is_factory_; }
  bool hasLastUpdateMillis() const { return last_update_millis_.has_value(); }
  int64_t getLastUpdateMillis() const { return last_update_millis_.value_or(0); }
  int64_t getVersionCode() const { return version_code_; }

  std::string module_name_;
  bool is_active_ = false;
  bool is_factory_ = false;
  std::optional<int64_t> last_update_millis_;
  int64_t version_code_ = 0;
};

class ApexInfoList {
 public:
  const std::vector<ApexInfo>& getApexInfo() const { return apex_info_; }
  std::vector<ApexInfo> apex_info_;
};

// Reads and parses the file. Returns nullopt if it cannot be read or is not
// an <apex-info-list> document.
inline std::optional<ApexInfoList> readApexInfoList(const char* path) {
  tinyxml2::XMLDocument doc;
  if (doc.LoadFile(path) != tinyxml2::XML_SUCCESS) return std::nullopt;
  const tinyxml2::XMLElement* root = doc.FirstChildElement("apex-info-list");
  if (root == nullptr) return std::nullopt;
  ApexInfoList list;
  for (const tinyxml2::XMLElement* e = root->FirstChildElement("apex-info"); e != nullptr;
       e = e->NextSiblingElement("apex-info")) {
    ApexInfo info;
    if (const char* n = e->Attribute("moduleName")) info.module_name_ = n;
    e->QueryBoolAttribute("isActive", &info.is_active_);
    e->QueryBoolAttribute("isFactory", &info.is_factory_);
    int64_t v = 0;
    if (e->QueryInt64Attribute("lastUpdateMillis", &v) == tinyxml2::XML_SUCCESS) {
      info.last_update_millis_ = v;
    }
    e->QueryInt64Attribute("versionCode", &info.version_code_);
    list.apex_info_.push_back(std::move(info));
  }
  return list;
}

}  // namespace apex
}  // namespace android
}  // namespace com
