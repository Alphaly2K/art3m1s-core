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
local function delete_save(path, log_failure)
    if type(path) ~= 'string' or path == '' then return nil, 'missing delete target', 'denied' end
    local ok, message, kind = yield(h.delete_save(path))
    if not ok then
        if log_failure then h.warn_delete(path, message) end
        return nil, message, kind
    end
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
local function file_query(self, params, resolved)
    local system = params.system
    local path = h.query_path(params, resolved)
    if tostring(params.save) == '1' and not path:find(':', 1, true)
        and path:sub(1, 1) ~= '/' and path:sub(1, 1) ~= '\\' then
        local prefix = self:var('s.savepath') .. '/'
        local normalized = path:gsub('\\', '/')
        if normalized:sub(1, #prefix):lower() ~= prefix:lower() then path = prefix .. path end
    end
    local value
    if system == 'file_exist' or system == 'file_exists' then
        if path:lower():sub(-4) == '.exe' then value = true
        else
            local ok, exists = yield(h.exists(path))
            value = ok and exists or false
        end
    elseif system == 'file_crc' then
        if path:lower():sub(-4) == '.exe' then
            h.complete_query(params, nil, true)
            return
        end
        value = read(path)
    elseif system == 'file_update_time' then
        local ok, time = yield(h.mtime(path))
        if ok then value = time end
    else error('unsupported resumed file query', 2) end
    h.complete_query(params, value)
end
return {
    io = io, flush = flush, close = close,
    file_query = file_query, delete_save = delete_save,
    tag = function(self, params, ...)
        if type(params) == "table" and params[1] == "var" then
            local s = params.system
            if s == "file_exist" or s == "file_exists" or s == "file_crc" or s == "file_update_time" then
                return file_query(self, params)
            end
        end
        return h.tag(self, params, ...)
    end,
    file = function(self, path)
        if type(path) == 'table' then
            local command = path.command
            if command == 'delete' then return delete_save(path.target) end
            if command == 'copy' or command == 'move' then
                local src, dst = path.src, path.dst
                if type(src) ~= 'string' or type(dst) ~= 'string' then
                    return nil, 'missing source or destination', 'denied'
                end
                local ok, message, kind = yield(h.transfer_save(src, dst, command == 'move'))
                if not ok then return nil, message, kind end
                return nil
            end
        end
        if type(path) ~= "string" then error("resumable e:file requires a path", 2) end
        local data = read(path)
        return data
    end,
    include = function(self, path)
        local data, message = read(path)
        if data == nil then error(message, 2) end
        return h.compile(data, path)()
    end,
    create_emote_layer = function(self, params)
        local id, path, width, height = h.emote_model_args(params)
        local data, message = read(path)
        if data == nil then error(message, 2) end
        return h.install_emote_model(id, path, width, height, data)
    end,
    exists = function(self, path)
        local ok, exists = yield(h.exists(path))
        return ok and exists or false
    end,
}
