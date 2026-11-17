local h = ...
local yield = coroutine.yield
local function read(path)
    local ok, data, kind = yield(h.read(path))
    if ok then return data end
    return nil, data, kind
end
local function write(path, data)
    return yield(h.write(path, data))
end
local function flush(file)
    local path, data = h.snapshot(file)
    if path then
        local ok, message = write(path, data)
        if not ok then error(message, 2) end
        h.ack(file)
    end
    return true
end
local function close(file)
    if not h.closed(file) then
        flush(file)
        h.close(file)
    end
    return true
end
local io = {}
function io.open(path, mode)
    mode = mode or "r"
    if not ({r=1, rb=1, ["r+"]=1, ["r+b"]=1, ["rb+"]=1,
             w=1, wb=1, ["w+"]=1, ["w+b"]=1, ["wb+"]=1,
             a=1, ab=1, ["a+"]=1, ["a+b"]=1, ["ab+"]=1})[mode] then
        return nil, "unsupported io.open mode: " .. tostring(mode)
    end
    local first = string.sub(mode, 1, 1)
    local data, message, kind
    if first == "w" then
        local ok
        ok, message = write(path, "")
        if not ok then return nil, message end
        data = ""
    else
        data, message, kind = read(path)
        if data == nil and first == "a" and kind == "not_found" then
            local ok
            ok, message = write(path, "")
            if not ok then return nil, message end
            data = ""
        end
        if data == nil then return nil, message end
    end
    return h.file(path, mode, data)
end
function io.close(file)
    if file then return close(file) end
    return true
end
function io.type(file)
    if type(file) ~= "userdata" then return nil end
    local ok, closed = pcall(h.closed, file)
    if not ok then return nil end
    return closed and "closed file" or "file"
end
function io.lines(path)
    local file, message = io.open(path, "r")
    if not file then error(message, 2) end
    return function()
        local line = file:read()
        if line == nil then close(file) end
        return line
    end
end
return {
    io = io, flush = flush, close = close,
    tag = function(self, params, ...)
        if type(params) == "table" and params[1] == "var" then
            local s = params.system
            if s == "file_exist" or s == "file_exists" or s == "file_crc" or s == "file_update_time" then
                error("file var queries require the resumable scenario dispatcher", 2)
            end
        end
        return h.tag(self, params, ...)
    end,
    file = function(self, path)
        if type(path) ~= "string" then error("resumable e:file requires a path", 2) end
        local data = read(path)
        return data
    end,
    include = function(self, path)
        local data, message = read(path)
        if data == nil then error(message, 2) end
        return h.compile(data, path)()
    end,
    exists = function(self, path)
        local ok, exists = yield(h.exists(path))
        return ok and exists or false
    end,
}
