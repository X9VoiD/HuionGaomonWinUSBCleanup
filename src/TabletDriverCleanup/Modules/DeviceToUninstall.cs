using System.Diagnostics.CodeAnalysis;

namespace TabletDriverCleanup.Modules;

public class DeviceToUninstall
{
    public string FriendlyName { get; }
    public string DeviceDescription { get; }
    public string? ManufacturerName { get; }
    public string? HardwareId { get; }
    public Guid? ClassGuid { get; }

    public string? ReplacementDriver { get; }
    public bool RemoveDevice => ReplacementDriver == null;

    public DeviceToUninstall(
        string friendlyName,
        [StringSyntax(StringSyntaxAttribute.Regex)] string deviceDescription,
        [StringSyntax(StringSyntaxAttribute.Regex)] string? manufacturerName = null,
        [StringSyntax(StringSyntaxAttribute.Regex)] string? hardwareId = null,
        Guid? classGuid = null)
    {
        FriendlyName = friendlyName;
        DeviceDescription = deviceDescription;
        ManufacturerName = manufacturerName;
        HardwareId = hardwareId;
        ClassGuid = classGuid;
    }
}