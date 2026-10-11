namespace Fizzy.McapSharp;

public sealed partial class McapWriter
{
    /// <summary>Writes the current fields of an owned metadata record synchronously without retaining it.</summary>
    public void WriteMetadata(McapMetadata metadata)
    {
        ArgumentNullException.ThrowIfNull(metadata);
        WriteMetadata(metadata.Name, metadata.Values);
    }

    /// <summary>Writes the current attachment fields and consumes its payload synchronously. Does not retain the object or payload.</summary>
    public void WriteAttachment(McapAttachment attachment)
    {
        ArgumentNullException.ThrowIfNull(attachment);
        ArgumentNullException.ThrowIfNull(attachment.Data);
        WriteAttachment(attachment.Name, attachment.MediaType, attachment.LogTime, attachment.CreateTime, attachment.Data);
    }
}
